//! With the `bundled-postgres` feature, embeds the PostgreSQL bundle named by
//! `CANNERY_POSTGRES_BUNDLE` (a `.tar.zst` from dev/build-postgres-bundle.sh)
//! and the digest from its `.sha256` sidecar. Without the variable the build
//! succeeds with a warning and the binary reports that it has no bundle, so
//! `--all-features` checks need no payload.
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(cannery_bundled_postgres)");
    println!("cargo::rerun-if-env-changed=CANNERY_POSTGRES_BUNDLE");
    if std::env::var_os("CARGO_FEATURE_BUNDLED_POSTGRES").is_none() {
        return;
    }
    let Some(bundle) = std::env::var_os("CANNERY_POSTGRES_BUNDLE").map(PathBuf::from) else {
        println!(
            "cargo::warning=bundled-postgres without CANNERY_POSTGRES_BUNDLE: no PostgreSQL is embedded"
        );
        return;
    };
    let mut sidecar = bundle.clone().into_os_string();
    sidecar.push(".sha256");
    let sidecar = PathBuf::from(sidecar);
    println!("cargo::rerun-if-changed={}", bundle.display());
    println!("cargo::rerun-if-changed={}", sidecar.display());
    let digest = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_ascii_lowercase))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
    let (Some(digest), true) = (digest, bundle.is_absolute() && bundle.is_file()) else {
        println!(
            "cargo::error=CANNERY_POSTGRES_BUNDLE must be an absolute path to a bundle with a <bundle>.sha256 file next to it"
        );
        return;
    };
    println!(
        "cargo::rustc-env=CANNERY_POSTGRES_BUNDLE_PATH={}",
        bundle.display()
    );
    println!("cargo::rustc-env=CANNERY_POSTGRES_BUNDLE_SHA256={digest}");
    println!("cargo::rustc-cfg=cannery_bundled_postgres");
}
