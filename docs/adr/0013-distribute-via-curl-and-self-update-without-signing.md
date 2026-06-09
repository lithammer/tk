# Distribute via curl|sh and tk self-update without signing in v1

Step 1 of release distribution serves bare-binary release assets from
GitHub Releases through a POSIX install script at
`github.com/lithammer/tk/releases/latest/download/install.sh` and
a built-in `tk self-update` that queries the Releases API for the
latest tag. The trust root is TLS + GitHub; an unsigned `SHA256SUMS`
shipped alongside the binaries shares its trust root with them and
would only catch corruption, not tampering, so v1 publishes neither
checksums nor signatures. The `man/tk.1` manpage is embedded into the
binary at compile time and placed by `tk manpage --install` so the
release asset stays a bare binary. A PowerShell install script at
`github.com/lithammer/tk/releases/latest/download/install.ps1`
mirrors the POSIX installer for Windows: same TLS + GitHub trust root,
same smoke-`--version` verification, and — for the same trust-root
reasoning — no checksum or signature. Homebrew/Scoop manifests and
minisign signing are deferred to follow-up tickets.

ADR-0029 supersedes the original `raw.githubusercontent.com/.../main/`
install-script URLs recorded here: the install scripts now ship as
assets on the immutable latest Release, so the bootstrap command is
pinned to a published release rather than a mutable `main` path.

## Considered Options

Plain checksums without signing were rejected as a half-measure since
the verification key would have to come from a different trust root to
add real security. Tarballs containing binary plus manpage were
rejected because the install script and `tk self-update` would pay
per-install extraction cost in exchange for `man tk` working, which is
not the primary doc surface for an agent-first CLI.

## Consequences

`tk` gains network capability (direct HTTPS to `github.com` and
`api.github.com`), changing the testing surface to require an
injectable HTTP client (or a subprocess `curl` runner) alongside the
existing subprocess runner. The binary embeds its compile-time triple
so a musl install upgrades to musl and a glibc install upgrades to
glibc; variant switching goes through re-running the install script
with `TK_LINUX_ABI=gnu` or equivalent.
