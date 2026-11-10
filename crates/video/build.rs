//! Compiles the AVFoundation encoder helper (`encoder.swift`) with the system's Swift compiler.

use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), String> {
    println!("cargo::rerun-if-changed=encoder.swift");
    println!("cargo::rerun-if-env-changed=LAPSEWERK_BUNDLE");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?).join("lapsewerk-encode");
    let mut swiftc = Command::new("xcrun");
    swiftc.args(["swiftc", "-O"]);
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        // macOS 15: the sRGB transfer tag for video.
        swiftc.args(["-target", "arm64-apple-macosx15.0"]);
    }
    let status = swiftc
        .arg("-o")
        .arg(&out)
        .arg("encoder.swift")
        .status()
        .map_err(|e| format!("cannot run the Swift compiler (Xcode command line tools): {e}"))?;
    if !status.success() {
        return Err(format!("swiftc failed: {status}"));
    }
    // An app bundle carries the helper beside the programs: no build path baked in then.
    if std::env::var_os("LAPSEWERK_BUNDLE").is_none() {
        println!("cargo::rustc-env=LAPSEWERK_ENCODER_BUILT={}", out.display());
    }
    Ok(())
}
