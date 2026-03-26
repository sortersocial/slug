//! Release CI sets `SLUG_VERSION` (semver without `v` prefix) so `slugsocial --version`
//! matches the published npm/PyPI package. Local builds fall back to `CARGO_PKG_VERSION`.

fn main() {
    let version = std::env::var("SLUG_VERSION").unwrap_or_else(|_| {
        std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION must be set")
    });
    println!("cargo:rustc-env=SLUG_CLI_VERSION={version}");
    println!("cargo:rerun-if-env-changed=SLUG_VERSION");
}
