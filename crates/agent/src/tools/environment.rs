//! Passive, bounded environment discovery shared with shell resolution.
//! Detection means a candidate executable exists, not that invoking it or
//! managing a service has been verified.

use std::path::{Path, PathBuf};

use remote_ops_protocol::{ShellKind, execution::MAX_EXECUTION_PATH_BYTES};
use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};

pub const MAX_PATH_DIRECTORIES: usize = 128;
pub const MAX_ENVIRONMENT_UTILITIES: usize = 24;

#[derive(Clone, Debug)]
pub(super) struct ShellEntry {
    pub kind: ShellKind,
    pub program: PathBuf,
}

pub(super) struct ProgramSearch {
    directories: Vec<PathBuf>,
    truncated: bool,
    ignored_relative_paths: bool,
}

impl ProgramSearch {
    pub fn new() -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        Self::from_directories(std::env::split_paths(&path))
    }

    fn from_directories(directories: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut search = Self {
            directories: Vec::new(),
            truncated: false,
            ignored_relative_paths: false,
        };
        for (index, directory) in directories.into_iter().enumerate() {
            if index == MAX_PATH_DIRECTORIES {
                search.truncated = true;
                break;
            }
            if !directory.is_absolute() {
                search.ignored_relative_paths = true;
                continue;
            }
            if display_path(&directory).is_none() {
                search.truncated = true;
                continue;
            }
            search.directories.push(directory);
        }
        search
    }

    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.directories
            .iter()
            .map(|directory| {
                #[cfg(windows)]
                {
                    directory.join(format!("{name}.exe"))
                }
                #[cfg(not(windows))]
                {
                    directory.join(name)
                }
            })
            .find(|candidate| is_executable(candidate))
    }

    #[cfg(unix)]
    fn find_with_fallback(&self, name: &str, fallbacks: &[&str]) -> Option<PathBuf> {
        self.find(name)
            .or_else(|| first_executable(fallbacks.iter().map(PathBuf::from)))
    }
}

fn display_path(path: &Path) -> Option<String> {
    path.to_str()
        .filter(|path| path.len() <= MAX_EXECUTION_PATH_BYTES)
        .map(str::to_string)
}

fn is_executable(path: &Path) -> bool {
    if display_path(path).is_none() {
        return false;
    }
    std::fs::metadata(path)
        .map(|metadata| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        })
        .unwrap_or(false)
}

fn first_executable(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| is_executable(path))
}

#[cfg(windows)]
fn windows_directory() -> Option<PathBuf> {
    let mut buffer = [0u16; 4096];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}

fn shell_program(kind: ShellKind, search: &ProgramSearch) -> Option<PathBuf> {
    match kind {
        #[cfg(unix)]
        ShellKind::Sh => first_executable([PathBuf::from("/bin/sh")]).or_else(|| search.find("sh")),
        #[cfg(unix)]
        ShellKind::Bash => search.find_with_fallback("bash", &["/bin/bash", "/usr/bin/bash"]),
        #[cfg(unix)]
        ShellKind::Busybox => search.find_with_fallback(
            "busybox",
            &["/bin/busybox", "/usr/bin/busybox", "/sbin/busybox"],
        ),
        ShellKind::Pwsh => search.find("pwsh").or_else(|| {
            #[cfg(windows)]
            {
                std::env::var_os("ProgramFiles")
                    .map(PathBuf::from)
                    .and_then(|path| {
                        first_executable([path.join("PowerShell").join("7").join("pwsh.exe")])
                    })
            }
            #[cfg(not(windows))]
            {
                first_executable([
                    PathBuf::from("/usr/bin/pwsh"),
                    PathBuf::from("/usr/local/bin/pwsh"),
                ])
            }
        }),
        #[cfg(windows)]
        ShellKind::Powershell => windows_directory().and_then(|directory| {
            first_executable([directory
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")])
        }),
        #[cfg(windows)]
        ShellKind::Cmd => windows_directory()
            .and_then(|directory| first_executable([directory.join("System32").join("cmd.exe")])),
        #[cfg(windows)]
        ShellKind::GitBash => first_executable([
            std::env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
                .join("Git")
                .join("bin")
                .join("bash.exe"),
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        ]),
        _ => None,
    }
}

fn shell_catalog(search: &ProgramSearch) -> Vec<ShellEntry> {
    ShellKind::ALL
        .into_iter()
        .filter_map(|kind| shell_program(kind, search).map(|program| ShellEntry { kind, program }))
        .collect()
}

fn native_shell(catalog: &[ShellEntry], windows: bool) -> Option<&ShellEntry> {
    let order: &[ShellKind] = if windows {
        &[ShellKind::Pwsh, ShellKind::Powershell]
    } else {
        &[ShellKind::Sh]
    };
    order
        .iter()
        .find_map(|kind| catalog.iter().find(|entry| &entry.kind == kind))
}

pub(super) fn resolve_shell(requested: ShellKind) -> AgentResult<ShellEntry> {
    let catalog = shell_catalog(&ProgramSearch::new());
    let entry = if requested == ShellKind::Native {
        native_shell(&catalog, cfg!(windows))
    } else {
        catalog.iter().find(|entry| entry.kind == requested)
    };
    entry.cloned().ok_or_else(|| AgentError::unsupported(format!("shell {} is not detected on this platform; inspect environment.shells and select an available shell explicitly", requested.name())))
}

pub fn environment() -> AgentResult<Value> {
    if !cfg!(any(unix, windows)) {
        return Err(AgentError::unsupported(
            "environment requires Unix or Windows",
        ));
    }
    let search = ProgramSearch::new();
    let catalog = shell_catalog(&search);
    let default_shell = native_shell(&catalog, cfg!(windows)).map(|entry| entry.kind.name());
    #[cfg(windows)]
    let tools: &[&str] = &[
        "sc", "schtasks", "tar", "curl", "git", "python3", "python", "busybox",
    ];
    #[cfg(not(windows))]
    let tools: &[&str] = &[
        "busybox",
        "toybox",
        "systemctl",
        "rc-service",
        "rc-status",
        "service",
        "sv",
        "s6-svc",
        "procd",
        "launchctl",
        "ip",
        "ifconfig",
        "ss",
        "netstat",
        "ps",
        "tar",
        "gzip",
        "curl",
        "wget",
        "git",
        "python3",
        "python",
        "make",
        "cc",
    ];
    let utilities = tools
        .iter()
        .take(MAX_ENVIRONMENT_UTILITIES)
        .filter_map(|name| {
            let path = search.find(name);
            #[cfg(unix)]
            let path = path.or_else(|| {
                first_executable(
                    ["/bin", "/sbin", "/usr/bin", "/usr/sbin"]
                        .map(|directory| Path::new(directory).join(name)),
                )
            });
            let path = path?;
            Some(json!({"name":name,"program":display_path(&path)}))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"family":std::env::consts::FAMILY},
        "cwd":std::env::current_dir().ok().as_deref().and_then(display_path),
        "shells":{
            "default":default_shell,
            "items":catalog.iter().map(|entry| json!({"name":entry.kind,"program":display_path(&entry.program)})).collect::<Vec<_>>(),
            "detection":"file_metadata", "truncated":search.truncated
        },
        "init_system":init_system_snapshot(),
        "utilities":{"available":true,"items":utilities,"truncated":search.truncated},
        "search":{"path_directories_scanned":search.directories.len(),"path_truncated":search.truncated,"ignored_relative_paths":search.ignored_relative_paths},
        "execution":{"stdin":"closed","output_encoding":"utf-8-lossy","unix_mode":cfg!(unix),"release_deployment":cfg!(unix),"process_tree":if cfg!(windows) {"job_object"} else {"process_group"}}
    }))
}

pub(super) fn init_system_snapshot() -> Value {
    #[cfg(target_os = "linux")]
    {
        let comm = super::system::bounded_text("/proc/1/comm", 256)
            .filter(|(_, truncated)| !truncated)
            .map(|(text, _)| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let executable = std::fs::read_link("/proc/1/exe")
            .ok()
            .as_deref()
            .and_then(display_path);
        let name = linux_init_name(comm.as_deref(), executable.as_deref());
        json!({"name":name,"pid1_comm":comm})
    }
    #[cfg(target_os = "macos")]
    {
        json!({"name":"launchd","pid1_comm":"launchd"})
    }
    #[cfg(windows)]
    {
        json!({"name":"windows-service-control-manager","pid1_comm":null})
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        json!({"name":null,"pid1_comm":null})
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_init_name<'a>(comm: Option<&'a str>, executable: Option<&str>) -> Option<&'a str> {
    if executable
        .and_then(|path| Path::new(path).file_name())
        .is_some_and(|name| name == "busybox")
    {
        Some("busybox-init")
    } else {
        comm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_never_falls_back_between_command_languages() {
        let mut catalog = vec![
            ShellEntry {
                kind: ShellKind::Cmd,
                program: "cmd.exe".into(),
            },
            ShellEntry {
                kind: ShellKind::Busybox,
                program: "/bin/busybox".into(),
            },
        ];
        assert!(native_shell(&catalog, true).is_none());
        assert!(native_shell(&catalog, false).is_none());
        catalog.push(ShellEntry {
            kind: ShellKind::Powershell,
            program: "powershell.exe".into(),
        });
        assert_eq!(
            native_shell(&catalog, true).unwrap().kind,
            ShellKind::Powershell
        );
        catalog.push(ShellEntry {
            kind: ShellKind::Pwsh,
            program: "pwsh.exe".into(),
        });
        assert_eq!(native_shell(&catalog, true).unwrap().kind, ShellKind::Pwsh);
    }

    #[test]
    fn init_identity_does_not_guess_from_installed_service_tools() {
        assert_eq!(
            linux_init_name(Some("init"), Some("/bin/busybox")),
            Some("busybox-init")
        );
        assert_eq!(linux_init_name(Some("procd"), None), Some("procd"));
        assert_eq!(
            linux_init_name(Some("systemd"), Some("/usr/lib/systemd/systemd")),
            Some("systemd")
        );
        assert_eq!(linux_init_name(None, None), None);
    }

    #[test]
    fn program_search_is_bounded_and_ignores_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let search = ProgramSearch::from_directories(std::iter::once(PathBuf::from(".")).chain(
            std::iter::repeat_n(root.path().to_path_buf(), MAX_PATH_DIRECTORIES + 1),
        ));
        assert!(search.truncated);
        assert!(search.ignored_relative_paths);
        assert!(search.directories.len() <= MAX_PATH_DIRECTORIES);
        let file = root.path().join(if cfg!(windows) {
            "fixture.exe"
        } else {
            "fixture"
        });
        std::fs::write(&file, b"candidate").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(search.find("fixture").is_none());
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(search.find("fixture"), Some(file));
    }
}
