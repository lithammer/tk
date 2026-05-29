# Release artifacts cross-compile from a single Linux host

Release builds produce all five supported triples from one
`ubuntu-latest` GitHub Actions runner. The active build (Rust) uses
`cargo-zigbuild`, which keeps Zig as the C cross-compiler/linker for
bundled SQLite; the toolchain (Rust + Zig CC pin) is recorded so the
same source revision produces byte-identical outputs across rebuilds
(Level-2 reproducibility). Smoke verification then runs on per-platform
native GitHub runners against the cross-compiled artifact, so what
gets tested is the shipped bytes, not a fresh native rebuild.

## Considered Options

A native-per-platform CI shape — one job per OS each running its own
native build — was rejected because `tk` needs no platform-specific
toolchain step (no Apple notarization, no MSVC link, no Windows
resource compiler), so host would be an implicit variable in the
artifact with no compensating benefit.

## Consequences

The release matrix is five triples: `x86_64-unknown-linux-musl`,
`x86_64-unknown-linux-gnu` (glibc 2.28 floor), `aarch64-unknown-linux-musl`,
`aarch64-apple-darwin`, and `x86_64-pc-windows-gnu`. ARM64 Windows is
deferred indefinitely: Rust ships no tier-2 mingw target for it
(`aarch64-pc-windows-gnullvm` is tier-3, needing `-Zbuild-std` on stable)
and the `windows-11-arm` smoke runner is in GitHub Actions preview. The
release-publish step gates upload on smoke success, so a triple whose smoke
fails is omitted from that release rather than blocking it.
