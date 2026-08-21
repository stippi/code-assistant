//! Build script that captures version/build information at compile time and
//! exposes it to the crate via `cargo:rustc-env` variables. The values mirror
//! the kind of metadata goreleaser embeds into Go binaries (commit, dirty
//! tree, build timestamp, toolchain, target triple).
//!
//! The corresponding runtime accessors live in `src/version.rs`.

use std::process::Command;

fn main() {
    // Re-run the build script when the git state that feeds the commit/dirty
    // fields changes. Working-tree edits that don't touch these files won't
    // force a rerun, so the dirty flag / build timestamp reflect the last
    // build that observed a relevant change — accurate for clean CI/release
    // builds (which always start from a fresh checkout).
    for path in [
        "../../.git/HEAD",
        "../../.git/index",
        "../../.git/refs",
        "build.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    // Short git commit hash (full SHA). "unknown" when git is unavailable
    // (e.g. building from a source tarball without a .git directory).
    let commit = run_git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_GIT_COMMIT={commit}");

    // Whether the working tree had uncommitted changes at build time.
    let dirty = run_git(&["status", "--porcelain"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false);
    println!(
        "cargo:rustc-env=BUILD_GIT_DIRTY={}",
        if dirty { "dirty" } else { "clean" }
    );

    // Build timestamp in RFC3339 UTC (e.g. 2026-08-20T17:01:00Z).
    let built = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    println!("cargo:rustc-env=BUILD_TIMESTAMP={built}");

    // Rust compiler version, e.g. "rustc 1.83.0 (90b35a623 2024-11-26)".
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_RUSTC_VERSION={rustc_version}");

    // Target triple the binary is being built for, e.g. "aarch64-apple-darwin".
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_TARGET={target}");
}

/// Run a git subcommand and return its trimmed stdout on success.
fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
