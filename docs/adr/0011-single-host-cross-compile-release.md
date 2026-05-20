# Release artifacts cross-compile from a single Linux host

Release builds produce all six supported triples from one
`ubuntu-latest` GitHub Actions runner via `zig build release`, with
Zig 0.16.0 pinned exactly so the same source revision produces
byte-identical outputs across rebuilds (Level-2 reproducibility).
Smoke verification then runs on per-platform native GitHub runners
against the cross-compiled artifact, so what gets tested is the
shipped bytes, not a fresh native rebuild.

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
