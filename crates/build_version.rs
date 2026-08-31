// Shared build-script logic for the proxy and agent binaries. Both
// crates/proxy/build.rs and crates/agent/build.rs include this file and
// call emit_version_env() to inject version, git and build-time metadata
// through cargo rustc-env at compile time.
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn app_version_code(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    (major * 10_000 + minor * 100 + patch).to_string()
}

pub fn emit_version_env() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=REMOTE_OPS_BUILD_TARGET={target}");
    println!("cargo:rustc-env=REMOTE_OPS_BUILD_PROFILE={profile}");

    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir.join("../..");
    let revision = std::env::var("REMOTE_OPS_GIT_REVISION")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| git_output(&workspace, &["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=REMOTE_OPS_GIT_REVISION={revision}");

    let short_hash = git_output(&workspace, &["rev-parse", "--short=7", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=REMOTE_OPS_GIT_SHORT_HASH={short_hash}");

    let version_code = std::env::var("CARGO_PKG_VERSION")
        .map(|version| app_version_code(&version))
        .unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=REMOTE_OPS_APP_VERSION_CODE={version_code}");

    let dirty = git_output(&workspace, &["status", "--porcelain"])
        .map(|_| "dirty")
        .unwrap_or("clean");
    println!("cargo:rustc-env=REMOTE_OPS_GIT_DIRTY={dirty}");

    let commit_date = git_output(&workspace, &["log", "-1", "--format=%cd", "--date=short"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=REMOTE_OPS_GIT_DATE={commit_date}");

    let now = chrono::Local::now();
    let offset_seconds = now.offset().local_minus_utc();
    let sign = if offset_seconds < 0 { "-" } else { "+" };
    let hours = offset_seconds.abs() / 3600;
    let minutes = (offset_seconds.abs() % 3600) / 60;
    let timezone = if minutes == 0 {
        format!("{sign}{hours}")
    } else {
        format!("{sign}{hours}:{minutes:02}")
    };
    println!(
        "cargo:rustc-env=REMOTE_OPS_BUILD_TIME={}",
        now.format("%Y-%m-%d %H:%M:%S")
    );
    println!("cargo:rustc-env=REMOTE_OPS_BUILD_TZ={timezone}");

    println!("cargo:rerun-if-env-changed=REMOTE_OPS_GIT_REVISION");
    for path in [".git/HEAD", ".git/refs", ".git/packed-refs"] {
        println!("cargo:rerun-if-changed={}", workspace.join(path).display());
    }
}
