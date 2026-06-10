//! Compile-time emitter for `TK_VERSION_STRING`.
//!
//! `tk self-update`'s smoke verification (per ADR-0013) runs the staged binary
//! with `--version` and requires both the tag and the embedded release triple
//! to appear as whole tokens in stdout. Per ADR-0029 the release pipeline is
//! the single source of both: the tag drives `TK_VERSION` and the build target
//! drives `TK_TRIPLE=<arch>-<os>-<env>` (e.g. `x86_64-unknown-linux-musl`).
//! Local `cargo build` injects neither, so the version falls back to the
//! `Cargo.toml` placeholder (`0.0.0`, never bumped — ADR-0029) and the triple
//! to `dev`, which is what fires the development refusal branch in
//! `commands::self_update`.
//!
//! Output: `TK_VERSION_STRING = "v<version> (<triple>)"`, consumed via
//! `env!("TK_VERSION_STRING")` by `cli`'s `#[command(version = …)]`.

fn main() {
    let triple = std::env::var("TK_TRIPLE").unwrap_or_else(|_| "dev".to_string());
    // The release version rides the git tag (ADR-0029); `CARGO_PKG_VERSION` is
    // only the local-build fallback, so the manifest version stays unbumped.
    let version = std::env::var("TK_VERSION").unwrap_or_else(|_| {
        std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION")
    });
    println!("cargo:rustc-env=TK_VERSION_STRING=v{version} ({triple})");
    println!("cargo:rustc-env=TK_EMBEDDED_TRIPLE={triple}");
    println!("cargo:rerun-if-env-changed=TK_TRIPLE");
    println!("cargo:rerun-if-env-changed=TK_VERSION");
}
