//! Build/version information captured at compile time by `build.rs`.
//!
//! Mirrors the metadata a Go binary built with goreleaser exposes: crate
//! version, git commit + clean/dirty tree state, build timestamp, and the
//! Rust toolchain / target triple. Consumed by the CLI `--version` output and
//! the GPUI "About" dialog.

/// Crate (semantic) version, e.g. `0.2.16`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Full git commit SHA the binary was built from, or `unknown`.
pub const GIT_COMMIT: &str = env!("BUILD_GIT_COMMIT");

/// Working-tree state at build time: `clean` or `dirty`.
pub const GIT_DIRTY: &str = env!("BUILD_GIT_DIRTY");

/// Build timestamp in RFC3339 UTC, e.g. `2026-08-20T17:01:00Z`.
pub const BUILD_TIMESTAMP: &str = env!("BUILD_TIMESTAMP");

/// Rust compiler version string, e.g. `rustc 1.83.0 (90b35a623 2024-11-26)`.
pub const RUSTC_VERSION: &str = env!("BUILD_RUSTC_VERSION");

/// Target triple, e.g. `aarch64-apple-darwin`.
pub const TARGET: &str = env!("BUILD_TARGET");

/// `true` for optimized (non-debug) builds.
pub fn is_release_build() -> bool {
    !cfg!(debug_assertions)
}

/// `true` when the git working tree was clean at build time.
pub fn git_tree_clean() -> bool {
    GIT_DIRTY == "clean"
}

/// Human label for the build profile, e.g. `release build` / `debug build`.
pub fn build_profile() -> &'static str {
    if is_release_build() {
        "release build"
    } else {
        "debug build"
    }
}

/// Human label for the git tree state, e.g. `clean git tree` / `dirty git tree`.
pub fn git_tree_label() -> &'static str {
    if git_tree_clean() {
        "clean git tree"
    } else {
        "dirty git tree"
    }
}

/// Single-line version summary as consumed by clap's `-V` output. clap
/// prepends the binary name, so this is just `v0.2.16 (release build)` and the
/// printed line becomes `code-assistant v0.2.16 (release build)`.
pub fn short() -> String {
    format!("v{VERSION} ({})", build_profile())
}

/// Multi-line version block for clap's `--version` output (clap prepends the
/// binary name to the first line), mirroring goreleaser-style metadata:
///
/// ```text
/// code-assistant v0.2.16 (release build)
/// commit: 6e24def… (clean git tree)
/// built:  2026-08-20T17:01:00Z
/// rust:   rustc 1.83.0 (90b35a623 2024-11-26) aarch64-apple-darwin
/// ```
pub fn long() -> String {
    format!(
        "{header}\n\
         commit: {commit} ({tree})\n\
         built:  {built}\n\
         rust:   {rustc} {target}",
        header = short(),
        commit = GIT_COMMIT,
        tree = git_tree_label(),
        built = BUILD_TIMESTAMP,
        rustc = RUSTC_VERSION,
        target = TARGET,
    )
}

/// Self-contained multi-line version block including the application name on
/// the first line (e.g. for clipboard copy from the About dialog).
pub fn long_with_name() -> String {
    format!("code-assistant {}", long())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_starts_with_version() {
        let s = short();
        assert!(s.starts_with(&format!("v{VERSION}")), "unexpected: {s}");
        assert!(s.ends_with("build)"), "unexpected: {s}");
    }

    #[test]
    fn long_has_all_fields() {
        let l = long();
        assert!(
            l.lines()
                .next()
                .unwrap()
                .starts_with(&format!("v{VERSION}"))
        );
        assert!(l.contains("commit: "));
        assert!(l.contains("built:  "));
        assert!(l.contains("rust:   "));
        assert!(l.contains(TARGET));
    }

    #[test]
    fn long_with_name_prefixes_app_name() {
        assert!(long_with_name().starts_with("code-assistant v"));
    }

    #[test]
    fn labels_match_flags() {
        assert_eq!(
            build_profile(),
            if cfg!(debug_assertions) {
                "debug build"
            } else {
                "release build"
            }
        );
        assert!(matches!(GIT_DIRTY, "clean" | "dirty"));
    }
}
