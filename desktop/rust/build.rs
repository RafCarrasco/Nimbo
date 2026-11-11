//! 1. Refuse the debug-only `test-host` feature in any release-derived profile, independent of
//!    `debug-assertions` overrides (policy.rs adds a compile_error! for the reverse case).
//! 2. Embed the existing Electron icon (desktop/icon.ico, unchanged) as resource 1 for Windows.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../icon.ico");
    println!("cargo:rerun-if-env-changed=PROFILE");
    if env::var_os("CARGO_FEATURE_TEST_HOST").is_some()
        && !env::var("PROFILE").is_ok_and(|p| p == "debug")
    {
        panic!(
            "NIMBO_TEST_HOST_FORBIDDEN_IN_RELEASE: the test-host feature is debug-only; release builds never carry test hooks"
        );
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets manifest dir"));
    let icon = manifest.join("../icon.ico");
    let icon = fs::canonicalize(&icon).expect("desktop/icon.ico must exist");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rc = out.join("nimbo.rc");
    let escaped = icon.display().to_string().replace('\\', "\\\\");
    fs::write(&rc, format!("1 ICON \"{escaped}\"\n")).expect("write resource script");
    let result = embed_resource::compile(&rc, embed_resource::NONE);
    if env::var("HOST").is_ok_and(|h| h.contains("windows")) {
        // A real Windows build without the icon resource is a build failure, not a warning.
        if let Err(error) = result.manifest_required() {
            panic!("icon resource compilation failed: {error}");
        }
    } else if let Err(error) = result.manifest_optional() {
        panic!("icon resource compilation failed: {error}");
    } else {
        println!(
            "cargo:warning=NOT a Windows build host: icon resource may be absent; this cross check is source/type evidence only, not a linkable artifact"
        );
    }
}
