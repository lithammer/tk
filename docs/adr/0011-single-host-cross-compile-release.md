# Release artifacts cross-compile from a single Linux host

Release builds produce all six supported triples — `x86_64-linux-musl`,
`x86_64-linux-gnu`, `aarch64-linux-musl`, `aarch64-macos`,
`x86_64-windows-gnu`, `aarch64-windows-gnu` — from one `ubuntu-latest`
GitHub Actions runner via a `zig build release` step. Zig 0.16.0 is
pinned exactly so the same source revision produces byte-identical
outputs across rebuilds. Combined with `ReleaseSafe`, stripped debug
info, and a `-Drelease-version=` build option, this gives Level-2
reproducibility (same recipe = same bytes across hosts, but not
Debian-grade third-party reproducibility, which would require
`SOURCE_DATE_EPOCH` and path/timestamp normalization).

The native-per-platform alternative — one CI job per OS each running its
own `zig build` — was rejected because it makes host the implicit
variable in the artifact and loses bit-identical rebuilds without
buying anything `tk` needs: there is no platform-specific toolchain step
(no Apple codesigning/notarization, no MSVC ABI link, no Windows
resource compiler), and Zig's cross-compilation supplies libc shims
natively.

Linkage policy is fixed per triple: musl builds are fully static (target
is containers, Alpine, minimal CI — not "supports old distros"); glibc
builds are dynamic with a 2.28 floor (RHEL 8 / Debian 10 / Ubuntu 18.04);
macOS uses dynamic `libSystem` with `-mmacos-version-min=11.0` because
Apple forbids static linking it; Windows uses `-static-libgcc` with
dynamic `msvcrt` so `tk.exe` ships as a single file.

Smoke verification runs per-platform on native GitHub runners by
downloading the cross-compiled artifact and running a minimal `tk init
/ add / list` scenario against it — testing the shipped bytes, not a
fresh native rebuild. The `aarch64-windows-gnu` smoke job is
`continue-on-error: true` because `windows-11-arm` is in GitHub Actions
preview; the release-publish step gates artifact upload on smoke
success, so that triple is best-effort per release until the preview
graduates.
