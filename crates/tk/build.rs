//! Compile-time emitter for `TK_VERSION_STRING`.
//!
//! `tk self-update`'s smoke verification (per ADR-0013) runs the staged binary
//! with `--version` and requires both the tag and the embedded release triple
//! to appear as whole tokens in stdout. The release pipeline injects
//! `TK_TRIPLE=<arch>-<os>-<env>` (e.g. `x86_64-unknown-linux-musl`) when
//! building shipped artifacts; local `cargo build` falls back to `dev` so the
//! development refusal branch in `commands::self_update` fires correctly.
//!
//! Output: `TK_VERSION_STRING = "v<crate-version> (<triple>)"`, consumed via
//! `env!("TK_VERSION_STRING")` by `cli`'s `#[command(version = …)]`.

fn main() {
    let triple = std::env::var("TK_TRIPLE").unwrap_or_else(|_| "dev".to_string());
    let pkg_version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    println!("cargo:rustc-env=TK_VERSION_STRING=v{pkg_version} ({triple})");
    println!("cargo:rustc-env=TK_EMBEDDED_TRIPLE={triple}");
    println!("cargo:rerun-if-env-changed=TK_TRIPLE");
}
