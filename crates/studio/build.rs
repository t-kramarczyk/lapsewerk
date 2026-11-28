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
    // The LightCraft that develops the photos (its commit in the lock file): frames of another
    // develop differently, so a delivery says it and a resume across it is refused.
    let lightcraft = std::fs::read_to_string("../../Cargo.lock")
        .ok()
        .and_then(|lock| {
            lock.split("[[package]]")
                .find(|p| p.contains("name = \"lightcraft-engine\""))
                .and_then(|p| p.split('#').nth(1).map(|r| r.chars().take(7).collect::<String>()))
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=LAPSEWERK_LIGHTCRAFT={lightcraft}");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    // A new commit (or checkout) changes the log.
    println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}
