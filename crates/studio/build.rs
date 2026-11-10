//! The build's commit (when built from a git checkout; otherwise the time of the build): analyses
//! measured by another build are measured anew, even under the same version number.

fn main() {
    let id = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // A tarball or a Nix build: each build stands for itself.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(|_| "unknown".to_owned(), |d| format!("built {}", d.as_secs()))
        });
    println!("cargo:rustc-env=LAPSEWERK_BUILD={id}");
    // A new commit (or checkout) changes the log.
    println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}
