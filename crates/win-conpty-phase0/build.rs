use std::process::Command;

fn main() {
    // Rebuild whenever HEAD changes so BUILD_GIT_REV stays fresh.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let rev = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_GIT_REV={rev}");

    // ISO-8601 UTC timestamp captured at build time (embedded in the
    // binary, printed at startup, referenced in the C-4 gate report).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_TIMESTAMP={ts}");
}
