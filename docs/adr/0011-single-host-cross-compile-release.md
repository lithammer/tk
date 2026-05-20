# Release artifacts cross-compile from a single Linux host

Release builds produce all six supported triples from one
`ubuntu-latest` GitHub Actions runner via `zig build release`, with
Zig 0.16.0 pinned exactly so the same source revision produces
byte-identical outputs across rebuilds (Level-2 reproducibility).
Linkage is fixed per triple: musl static for containers/Alpine, glibc
dynamic with a 2.28 floor (RHEL 8 / Debian 10 / Ubuntu 18.04), macOS
dynamic `libSystem` with `-mmacos-version-min=11.0` (Apple forbids
static linking it), Windows `-static-libgcc` + dynamic `msvcrt` so
`tk.exe` ships as a single file. Smoke verification runs on
per-platform native GitHub runners so what gets tested is the shipped
bytes, not a fresh native rebuild.

## Considered Options

A native-per-platform CI shape — one job per OS each running its own
`zig build` — was rejected because `tk` needs no platform-specific
toolchain step (no Apple notarization, no MSVC link, no Windows
resource compiler), so host would be an implicit variable in the
artifact with no compensating benefit.

## Consequences

`aarch64-windows-gnu` smoke is `continue-on-error: true` while
`windows-11-arm` is in GitHub Actions preview; the release-publish step
gates upload on smoke success, so that triple is best-effort per
release.
