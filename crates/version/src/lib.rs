//! Shared version-string rendering and `--version` argument detection for
//! the proxy and agent binaries. All build-time metadata is injected by this
//! crate's own build script through `crates/build_version.rs`.

pub const REPOSITORY_URL: &str = "https://github.com/jark006/RemoteOps";

/// Whether any argument requests the version (equivalent to `--version`/`-v`).
pub fn requests_version(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter()
        .any(|argument| argument == "--version" || argument == "-v")
}

/// Renders the single version line, e.g.
/// `v1.4.0 (10400) g31fbfda-dirty (2026-08-28) BuildTime 2026-08-31 21:09:52 UTC+8`.
pub fn version_line() -> String {
    let dirty = if env!("REMOTE_OPS_GIT_DIRTY") == "dirty" {
        "-dirty"
    } else {
        ""
    };
    format!(
        "v{} ({}) g{}{} ({}) BuildTime {} UTC{}",
        env!("CARGO_PKG_VERSION"),
        env!("REMOTE_OPS_APP_VERSION_CODE"),
        env!("REMOTE_OPS_GIT_SHORT_HASH"),
        dirty,
        env!("REMOTE_OPS_GIT_DATE"),
        env!("REMOTE_OPS_BUILD_TIME"),
        env!("REMOTE_OPS_BUILD_TZ"),
    )
}

/// Renders the full `--version` output for a binary, e.g.
/// `remote-ops-proxy v1.4.0 (...) ...` followed by the repository URL.
pub fn version_text(binary_name: &str) -> String {
    format!("{binary_name} {} {REPOSITORY_URL}\n", version_line())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_version_matches_both_flags() {
        assert!(requests_version(["--version".to_string()]));
        assert!(requests_version(["-v".to_string()]));
        assert!(!requests_version([
            "--listen".to_string(),
            "0.0.0.0:8022".to_string()
        ]));
    }

    #[test]
    fn version_text_reports_version_and_repository() {
        let text = version_text("remote-ops-test");
        let dirty = if env!("REMOTE_OPS_GIT_DIRTY") == "dirty" {
            "-dirty"
        } else {
            ""
        };
        assert!(text.starts_with("remote-ops-test "));
        assert!(text.contains(&format!(
            "v{} ({}) g{}{} ({}) BuildTime {} UTC{}",
            env!("CARGO_PKG_VERSION"),
            env!("REMOTE_OPS_APP_VERSION_CODE"),
            env!("REMOTE_OPS_GIT_SHORT_HASH"),
            dirty,
            env!("REMOTE_OPS_GIT_DATE"),
            env!("REMOTE_OPS_BUILD_TIME"),
            env!("REMOTE_OPS_BUILD_TZ"),
        )));
        assert!(text.contains(REPOSITORY_URL));
    }
}
