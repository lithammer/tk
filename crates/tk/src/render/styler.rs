//! Runtime policy gate for the named [`palette`](super::palette) entries.
//!
//! Carries one resolved [`ColorChoice`] per output stream (stdout, stderr);
//! commands styling output reach for a sub-styler via
//! [`Styler::for_stdout`] / [`Styler::for_stderr`] and treat the returned
//! [`SubStyler`] as the policy-aware emitter. The choice is resolved once
//! at startup from the env/TTY chain and threaded through `Deps`; commands
//! never re-resolve it. See ADR-0014.
//!
//! ## Disjoint-family close
//!
//! [`anstyle::Style::render_reset`] emits the universal `\x1b[0m` reset,
//! which would clobber any outer span and silently break the
//! disjoint-family nesting invariant ADR-0014 records. The [`Close`]
//! adapter emits family-specific SGR closes ([`Effects::BOLD`] /
//! [`Effects::DIMMED`] → `22`, foreground colour → `39`, …) in reverse
//! order of open so a multi-family style still nests safely.

use core::fmt;

use anstyle::{Effects, Style};

/// Whether styled output is emitted for one stream.
///
/// ADR-0014 ties tk to SGR escape codes only; the legacy Windows console
/// path is therefore treated as plain output ([`ColorChoice::Never`])
/// rather than carried as a third variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    /// Emit raw SGR escape codes.
    Always,
    /// Suppress SGR — the wrap/open/close path returns plain text and
    /// empty byte sequences. Scenario tests assert against this value
    /// (the harness defaults TTY detection to false per ADR-0014).
    Never,
}

/// Resolve the per-stream colour choice from the env/TTY inputs.
///
/// Precedence (matching ADR-0014's documented chain):
///
/// 1. `NO_COLOR` (any non-empty value) → [`ColorChoice::Never`].
/// 2. `CLICOLOR_FORCE` (any non-empty value) → [`ColorChoice::Always`].
/// 3. `is_tty` → [`ColorChoice::Always`] when true, otherwise
///    [`ColorChoice::Never`].
///
/// Caller is responsible for any platform-specific downgrade (e.g.
/// legacy Windows console without VT support) — see
/// [`apply_terminal_capability`] and [`resolve_styler_from_env`].
#[must_use]
pub fn resolve_choice(no_color: bool, clicolor_force: bool, is_tty: bool) -> ColorChoice {
    if no_color {
        ColorChoice::Never
    } else if clicolor_force || is_tty {
        ColorChoice::Always
    } else {
        ColorChoice::Never
    }
}

/// Honour ADR-0014's "legacy console → no-color" arm: an [`Always`]
/// choice on a TTY that cannot render SGR escapes downgrades to
/// [`Never`] so the user sees plain text rather than literal `^[[NNm`
/// bytes.
///
/// `vt_enabled` is the result of `anstyle_query::windows::enable_ansi_colors()`
/// (or any equivalent VT-mode probe) — `None` on non-Windows or when
/// the query is irrelevant, `Some(true)` if the console supports SGR
/// (including modern Windows Terminal with VT already on), `Some(false)`
/// for legacy `cmd.exe` where VT could not be enabled.
/// `term_supports_ansi` is `anstyle_query::term_supports_ansi_color()`
/// — `TERM` advertises an ANSI-capable terminal even when the OS
/// console doesn't, so an inherited xterm-style `TERM` rescues the
/// `Always` choice.
///
/// [`Always`]: ColorChoice::Always
/// [`Never`]: ColorChoice::Never
#[must_use]
pub fn apply_terminal_capability(
    base: ColorChoice,
    is_tty: bool,
    vt_enabled: Option<bool>,
    term_supports_ansi: bool,
) -> ColorChoice {
    if base != ColorChoice::Always {
        return base;
    }
    // Non-Windows or VT-query irrelevant → no downgrade.
    let Some(vt_ok) = vt_enabled else {
        return base;
    };
    if vt_ok {
        return base;
    }
    // VT could not be enabled. Allow `TERM` to override (an inherited
    // ANSI-capable terminal description means the user really does see
    // SGR somehow — typically a relayed pty), and only honour the
    // CLICOLOR_FORCE / non-TTY paths that brought us here when the
    // console isn't going to render bytes anyway.
    if term_supports_ansi || !is_tty {
        base
    } else {
        ColorChoice::Never
    }
}

/// Build a [`Styler`] from the current process env (`NO_COLOR`,
/// `CLICOLOR_FORCE`) and per-stream `IsTerminal` probes, then apply
/// platform-specific terminal-capability downgrades so legacy Windows
/// consoles without VT support see plain output (ADR-0014's
/// legacy-console → no-color arm). The process entrypoint calls this
/// once before locking stdout/stderr; commands receive the resolved
/// value via `Deps`.
#[must_use]
pub fn resolve_styler_from_env() -> Styler {
    use std::io::IsTerminal;
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let clicolor_force = std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty());
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    // `enable_ansi_colors` is `None` on non-Windows. On Windows it
    // attempts to flip the VT processing bit on the current console
    // (idempotent; the call has no effect on already-VT-enabled
    // consoles like Windows Terminal). One call suffices for both
    // streams because they share a console.
    let vt = anstyle_query::windows::enable_ansi_colors();
    let term_ansi = anstyle_query::term_supports_ansi_color();
    let stdout_base = resolve_choice(no_color, clicolor_force, stdout_tty);
    let stderr_base = resolve_choice(no_color, clicolor_force, stderr_tty);
    Styler {
        stdout: apply_terminal_capability(stdout_base, stdout_tty, vt, term_ansi),
        stderr: apply_terminal_capability(stderr_base, stderr_tty, vt, term_ansi),
    }
}

/// Process-wide styler carried on `cli::Deps`. Holds per-stream
/// [`ColorChoice`] values so a piped stdout (`tk list | less`) does not
/// silence colour on stderr.
#[derive(Debug, Clone, Copy)]
pub struct Styler {
    pub stdout: ColorChoice,
    pub stderr: ColorChoice,
}

impl Styler {
    /// All-plain styler. The default that scenario and command-handler
    /// tests use so byte-exact assertions hold without TTY mocking.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            stdout: ColorChoice::Never,
            stderr: ColorChoice::Never,
        }
    }

    /// Sub-styler bound to the stdout choice. Use when wrapping content
    /// destined for `deps.stdout`.
    #[must_use]
    pub const fn for_stdout(self) -> SubStyler {
        SubStyler {
            choice: self.stdout,
        }
    }

    /// Sub-styler bound to the stderr choice. The stderr palette has no
    /// entries in slice 0; only the plumbing is in place (ADR-0014).
    #[must_use]
    pub const fn for_stderr(self) -> SubStyler {
        SubStyler {
            choice: self.stderr,
        }
    }
}

/// Policy-aware emitter for one output stream. Returned by
/// [`Styler::for_stdout`] / [`Styler::for_stderr`]. Call sites use
/// [`SubStyler::wrap`] for single-span styled text or [`SubStyler::open`]
/// / [`SubStyler::close`] to bracket multi-write rows.
#[derive(Debug, Clone, Copy)]
pub struct SubStyler {
    choice: ColorChoice,
}

impl SubStyler {
    /// Wrap `text` in `style`'s SGR open + family-specific close. The
    /// returned [`Styled`] implements [`fmt::Display`] so call sites can
    /// slot it into format arguments (`write!(stdout, "{styled}")`).
    /// When the choice is [`ColorChoice::Never`], the wrapper carries
    /// `Style::new()` and emits the text only — byte-identical to plain
    /// output.
    #[must_use]
    pub fn wrap(self, style: Style, text: &str) -> Styled<'_> {
        Styled {
            style: self.gated(style),
            text,
        }
    }

    /// SGR open for `style`. Returns [`anstyle::Style`] directly so
    /// callers can `write!(f, "{}", open)` it. Use to bracket a
    /// multi-write outer span (e.g. dim the whole row for a blocked
    /// Item) while inner [`SubStyler::wrap`] calls handle individual
    /// spans. Under [`ColorChoice::Never`] the returned value is
    /// `Style::new()`, which renders to the empty string.
    #[must_use]
    pub fn open(self, style: Style) -> Style {
        self.gated(style)
    }

    /// Family-specific SGR close adapter for `style`. Pair with a prior
    /// `open(style)` at the end of a multi-write outer span. Under
    /// [`ColorChoice::Never`] the adapter renders to the empty string.
    #[must_use]
    pub fn close(self, style: Style) -> Close {
        Close(self.gated(style))
    }

    /// Apply the policy gate: pass `style` through under
    /// [`ColorChoice::Always`], or replace with `Style::new()` (the
    /// rendering identity) under [`ColorChoice::Never`].
    fn gated(self, style: Style) -> Style {
        match self.choice {
            ColorChoice::Always => style,
            ColorChoice::Never => Style::new(),
        }
    }
}

/// Format value returned by [`SubStyler::wrap`]. Writes `open` (via
/// [`Style`]'s [`fmt::Display`] impl), the text, then the
/// family-specific [`Close`] in one pass.
#[derive(Debug, Clone, Copy)]
pub struct Styled<'a> {
    style: Style,
    text: &'a str,
}

impl fmt::Display for Styled<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}{}", self.style, self.text, Close(self.style))
    }
}

/// Family-specific SGR close adapter. Emits the per-family close codes
/// matching the attributes set on the wrapped [`Style`], in reverse
/// order of [`Style`]'s `Display` impl so nested spans still close
/// inside-out. ADR-0014's disjoint-family invariant means well-written
/// palette entries land in a single family and the output is the same
/// single SGR pair the renderer would write by hand.
#[derive(Debug, Clone, Copy)]
pub struct Close(Style);

impl fmt::Display for Close {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_plain() {
            return Ok(());
        }
        let effects = self.0.get_effects();
        let any_underline_effect = effects.contains(Effects::UNDERLINE)
            || effects.contains(Effects::DOUBLE_UNDERLINE)
            || effects.contains(Effects::CURLY_UNDERLINE)
            || effects.contains(Effects::DOTTED_UNDERLINE)
            || effects.contains(Effects::DASHED_UNDERLINE);
        // Underline colour closes before underline effect: a `59` reset
        // returns the colour to default while leaving the line itself
        // active, which `24` then turns off.
        if self.0.get_underline_color().is_some() {
            f.write_str("\x1b[59m")?;
        }
        if any_underline_effect {
            f.write_str("\x1b[24m")?;
        }
        if self.0.get_bg_color().is_some() {
            f.write_str("\x1b[49m")?;
        }
        if self.0.get_fg_color().is_some() {
            f.write_str("\x1b[39m")?;
        }
        // SGR 22 closes BOLD and DIMMED together; emit once even if both
        // are set.
        if effects.contains(Effects::BOLD) || effects.contains(Effects::DIMMED) {
            f.write_str("\x1b[22m")?;
        }
        if effects.contains(Effects::ITALIC) {
            f.write_str("\x1b[23m")?;
        }
        if effects.contains(Effects::BLINK) {
            f.write_str("\x1b[25m")?;
        }
        if effects.contains(Effects::INVERT) {
            f.write_str("\x1b[27m")?;
        }
        if effects.contains(Effects::HIDDEN) {
            f.write_str("\x1b[28m")?;
        }
        if effects.contains(Effects::STRIKETHROUGH) {
            f.write_str("\x1b[29m")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::palette;

    #[test]
    fn resolve_choice_precedence_matches_adr_0014_chain() {
        assert_eq!(resolve_choice(true, true, true), ColorChoice::Never);
        assert_eq!(resolve_choice(false, true, false), ColorChoice::Always);
        assert_eq!(resolve_choice(false, false, true), ColorChoice::Always);
        assert_eq!(resolve_choice(false, false, false), ColorChoice::Never);
    }

    #[test]
    fn capability_downgrade_passes_through_when_not_windows() {
        // `vt_enabled = None` ≡ non-Windows; the chosen `Always` survives.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Always, true, None, false),
            ColorChoice::Always,
        );
        assert_eq!(
            apply_terminal_capability(ColorChoice::Never, true, None, false),
            ColorChoice::Never,
        );
    }

    #[test]
    fn capability_downgrade_passes_through_when_vt_succeeded() {
        // Modern Windows Terminal / VT-enabled cmd.exe: SGR is fine.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Always, true, Some(true), false),
            ColorChoice::Always,
        );
    }

    #[test]
    fn capability_downgrade_forces_never_on_legacy_console() {
        // Legacy cmd.exe: TTY but VT could not be enabled and TERM
        // doesn't advertise ANSI. ADR-0014 demands plain output.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Always, true, Some(false), false),
            ColorChoice::Never,
        );
    }

    #[test]
    fn capability_downgrade_respects_term_supports_ansi() {
        // VT-enable failed but TERM=xterm-256color in the env (e.g. a
        // relayed pty under Cygwin). Keep SGR — the bytes will reach
        // an ANSI-capable consumer somewhere.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Always, true, Some(false), true),
            ColorChoice::Always,
        );
    }

    #[test]
    fn capability_downgrade_keeps_non_tty_always() {
        // CLICOLOR_FORCE=1 with stdout piped: we asked for Always
        // explicitly, the pipe consumer (e.g. `less -R`) renders it.
        // VT-enable failure on the parent console is irrelevant.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Always, false, Some(false), false),
            ColorChoice::Always,
        );
    }

    #[test]
    fn capability_downgrade_leaves_never_alone() {
        // Never never upgrades.
        assert_eq!(
            apply_terminal_capability(ColorChoice::Never, true, Some(true), true),
            ColorChoice::Never,
        );
    }

    #[test]
    fn substyler_open_emits_style_when_always_empty_when_never() {
        let style = palette::HEADER;
        let on = SubStyler {
            choice: ColorChoice::Always,
        };
        let off = SubStyler {
            choice: ColorChoice::Never,
        };
        assert_eq!(format!("{}", on.open(style)), "\x1b[1m");
        assert_eq!(format!("{}", off.open(style)), "");
    }

    #[test]
    fn substyler_close_emits_family_close_when_always_empty_when_never() {
        let style = palette::HEADER;
        let on = SubStyler {
            choice: ColorChoice::Always,
        };
        let off = SubStyler {
            choice: ColorChoice::Never,
        };
        assert_eq!(format!("{}", on.close(style)), "\x1b[22m");
        assert_eq!(format!("{}", off.close(style)), "");
    }

    #[test]
    fn for_stderr_uses_stderr_choice_independently_of_stdout() {
        let styler = Styler {
            stdout: ColorChoice::Never,
            stderr: ColorChoice::Always,
        };
        let style = palette::KIND_BUG;
        let stdout_wrapped = format!("{}", styler.for_stdout().wrap(style, "TEXT"));
        let stderr_wrapped = format!("{}", styler.for_stderr().wrap(style, "TEXT"));
        assert_eq!(stdout_wrapped, "TEXT");
        assert_eq!(stderr_wrapped, "\x1b[31mTEXT\x1b[39m");
    }

    #[test]
    fn wrap_elides_open_close_when_choice_is_never() {
        let styler = Styler::plain();
        let style = palette::KIND_BUG;
        assert_eq!(
            format!("{}", styler.for_stdout().wrap(style, "TEXT")),
            "TEXT"
        );
    }

    #[test]
    fn wrap_emits_open_text_close_when_choice_is_always() {
        let styler = Styler {
            stdout: ColorChoice::Always,
            stderr: ColorChoice::Always,
        };
        let style = palette::HEADER;
        assert_eq!(
            format!("{}", styler.for_stdout().wrap(style, "TEXT")),
            "\x1b[1mTEXT\x1b[22m",
        );
    }

    #[test]
    fn wrap_plain_palette_entry_under_always_emits_only_text() {
        // Pin the no-escape contract for palette entries that resolve to
        // `Style::new()` (placeholders like `ID_EPIC`, `STATUS_OPEN`,
        // `PRIORITY_P2`). Both [`Style`]'s [`fmt::Display`] impl and
        // [`Close`] must elide their byte output for these.
        let styler = Styler {
            stdout: ColorChoice::Always,
            stderr: ColorChoice::Always,
        };
        for style in [palette::ID_EPIC, palette::STATUS_OPEN, palette::PRIORITY_P2] {
            assert_eq!(
                format!("{}", styler.for_stdout().wrap(style, "TEXT")),
                "TEXT",
            );
        }
    }

    #[test]
    fn close_covers_all_single_family_effects() {
        // Defends against the family_close ancestor's narrow coverage:
        // every single-family effect now closes with its own SGR rather
        // than panicking.
        let on = SubStyler {
            choice: ColorChoice::Always,
        };
        let cases: &[(Style, &str)] = &[
            (Style::new().bold(), "\x1b[22m"),
            (Style::new().dimmed(), "\x1b[22m"),
            (Style::new().italic(), "\x1b[23m"),
            (Style::new().underline(), "\x1b[24m"),
            (Style::new().blink(), "\x1b[25m"),
            (Style::new().invert(), "\x1b[27m"),
            (Style::new().hidden(), "\x1b[28m"),
            (Style::new().strikethrough(), "\x1b[29m"),
        ];
        for (style, expected) in cases {
            assert_eq!(format!("{}", on.close(*style)), *expected);
        }
    }

    #[test]
    fn close_emits_multi_family_in_reverse_open_order() {
        // bold + red opens as `\x1b[1m\x1b[31m`; close must emit `39`
        // (fg) before `22` (bold) so the inner family unwinds first.
        let on = SubStyler {
            choice: ColorChoice::Always,
        };
        let style = palette::HEADER.fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)));
        assert_eq!(format!("{}", on.close(style)), "\x1b[39m\x1b[22m");
    }

    /// Byte-exact pin of every palette entry's open / close pair under
    /// both choices, keeping the SGR contract verifiable without a TTY.
    #[test]
    fn palette_entries_emit_expected_bytes_under_each_choice() {
        struct Case {
            name: &'static str,
            style: Style,
            on_open: &'static str,
            on_close: &'static str,
        }
        let cases = [
            Case {
                name: "header",
                style: palette::HEADER,
                on_open: "\x1b[1m",
                on_close: "\x1b[22m",
            },
            Case {
                name: "id_epic",
                style: palette::ID_EPIC,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "id_ticket",
                style: palette::ID_TICKET,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "kind_bug",
                style: palette::KIND_BUG,
                on_open: "\x1b[31m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "kind_epic",
                style: palette::KIND_EPIC,
                on_open: "\x1b[35m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "status_open",
                style: palette::STATUS_OPEN,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "status_active",
                style: palette::STATUS_ACTIVE,
                on_open: "\x1b[33m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "status_done",
                style: palette::STATUS_DONE,
                on_open: "\x1b[32m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "blocked",
                style: palette::BLOCKED,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "blocked_row",
                style: palette::BLOCKED_ROW,
                on_open: "\x1b[2m",
                on_close: "\x1b[22m",
            },
            Case {
                name: "separator",
                style: palette::SEPARATOR,
                on_open: "\x1b[2m",
                on_close: "\x1b[22m",
            },
            Case {
                name: "priority_p0",
                style: palette::PRIORITY_P0,
                on_open: "\x1b[31m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "priority_p1",
                style: palette::PRIORITY_P1,
                on_open: "\x1b[33m",
                on_close: "\x1b[39m",
            },
            Case {
                name: "priority_p2",
                style: palette::PRIORITY_P2,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "priority_p3",
                style: palette::PRIORITY_P3,
                on_open: "",
                on_close: "",
            },
            Case {
                name: "priority_p4",
                style: palette::PRIORITY_P4,
                on_open: "",
                on_close: "",
            },
        ];
        let on = SubStyler {
            choice: ColorChoice::Always,
        };
        let off = SubStyler {
            choice: ColorChoice::Never,
        };
        for case in cases {
            assert_eq!(
                format!("{}", on.open(case.style)),
                case.on_open,
                "open mismatch for {}",
                case.name,
            );
            assert_eq!(
                format!("{}", on.close(case.style)),
                case.on_close,
                "close mismatch for {}",
                case.name,
            );
            assert_eq!(
                format!("{}", off.open(case.style)),
                "",
                "no-colour open should be empty for {}",
                case.name,
            );
            assert_eq!(
                format!("{}", off.close(case.style)),
                "",
                "no-colour close should be empty for {}",
                case.name,
            );
        }
    }
}
