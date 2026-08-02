use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use serde_json::json;

#[cfg(not(target_os = "linux"))]
use crate::error::{AgentError, AgentResult};

#[cfg(target_os = "linux")]
pub(super) const MAX_SYSTEM_FILE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_INTERFACES: usize = 128;
pub(super) const MAX_INTERFACE_ADDRESSES: usize = 512;
#[cfg(target_os = "linux")]
pub(super) const MAX_ROUTES: usize = 256;
#[cfg(unix)]
pub(super) const MAX_DNS_ENTRIES: usize = 16;
#[cfg(target_os = "linux")]
pub(super) const MAX_LISTENING_PORTS: usize = 512;
#[cfg(target_os = "linux")]
pub(super) const MAX_MOUNTS: usize = 256;
#[cfg(unix)]
const MAX_GROUPS: usize = 128;
const MAX_PATH_DIRECTORIES: usize = 128;
pub(super) const MAX_TOOLCHAINS: usize = 24;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::system_info;

#[derive(Default)]
struct InterfaceSummary {
    index: Option<u32>,
    up: bool,
    loopback: bool,
    point_to_point: bool,
    addresses: Vec<Value>,
}

#[cfg(unix)]
pub(super) fn bounded_text(path: impl AsRef<Path>, limit: usize) -> Option<(String, bool)> {
    let mut file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(limit.min(8192));
    file.by_ref()
        .take(u64::try_from(limit).ok()?.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Some((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

pub(super) fn collection(available: bool, items: Vec<Value>, truncated: bool) -> Value {
    json!({"available": available, "items": items, "truncated": truncated})
}

fn interface_snapshot() -> Value {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(_) => return collection(false, Vec::new(), false),
    };
    let mut summaries = BTreeMap::<String, InterfaceSummary>::new();
    let mut truncated = false;
    let mut address_count = 0usize;
    for interface in interfaces {
        if summaries.len() == MAX_INTERFACES && !summaries.contains_key(&interface.name) {
            truncated = true;
            continue;
        }
        let up = interface.is_oper_up();
        let loopback = interface.is_loopback();
        let point_to_point = interface.is_p2p();
        let summary = summaries.entry(interface.name).or_default();
        summary.index = summary.index.or(interface.index);
        summary.up |= up;
        summary.loopback |= loopback;
        summary.point_to_point |= point_to_point;
        if summary.addresses.len() >= MAX_INTERFACE_ADDRESSES
            || address_count >= MAX_INTERFACE_ADDRESSES
        {
            truncated = true;
            continue;
        }
        let (family, address, prefix_length, scope) = match interface.addr {
            if_addrs::IfAddr::V4(address) => (
                "ipv4",
                address.ip.to_string(),
                address.prefixlen,
                ip_scope(std::net::IpAddr::V4(address.ip)),
            ),
            if_addrs::IfAddr::V6(address) => (
                "ipv6",
                address.ip.to_string(),
                address.prefixlen,
                ip_scope(std::net::IpAddr::V6(address.ip)),
            ),
        };
        summary.addresses.push(json!({
            "family": family,
            "address": address,
            "prefix_length": prefix_length,
            "scope": scope
        }));
        address_count += 1;
    }
    let items = summaries
        .into_iter()
        .map(|(name, summary)| {
            let (mac_address, mtu) = interface_link_details(&name);
            json!({
                "name": name,
                "index": summary.index,
                "up": summary.up,
                "loopback": summary.loopback,
                "point_to_point": summary.point_to_point,
                "mac_address": mac_address,
                "mtu": mtu,
                "addresses": summary.addresses
            })
        })
        .collect();
    collection(true, items, truncated)
}

fn ip_scope(address: std::net::IpAddr) -> &'static str {
    if address.is_loopback() {
        "host"
    } else if address.is_unspecified() {
        "unspecified"
    } else if address.is_multicast() {
        "multicast"
    } else if match address {
        std::net::IpAddr::V4(address) => address.is_link_local(),
        std::net::IpAddr::V6(address) => address.is_unicast_link_local(),
    } {
        "link"
    } else {
        "global"
    }
}

#[cfg(target_os = "linux")]
fn interface_link_details(name: &str) -> (Option<String>, Option<u64>) {
    let base = Path::new("/sys/class/net").join(name);
    let mac_address = bounded_text(base.join("address"), 128)
        .map(|(value, _)| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mtu = bounded_text(base.join("mtu"), 64).and_then(|(value, _)| value.trim().parse().ok());
    (mac_address, mtu)
}

#[cfg(not(target_os = "linux"))]
fn interface_link_details(_name: &str) -> (Option<String>, Option<u64>) {
    (None, None)
}

#[cfg(unix)]
pub(super) fn resolv_conf_snapshot() -> Value {
    let Some((contents, file_truncated)) = bounded_text("/etc/resolv.conf", 64 * 1024) else {
        return json!({
            "available": false,
            "servers": [],
            "search_domains": [],
            "truncated": false
        });
    };
    let mut servers = Vec::new();
    let mut search_domains = Vec::new();
    let mut truncated = file_truncated;
    for line in contents.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("nameserver") => {
                if let Some(server) = fields.next() {
                    if servers.len() < MAX_DNS_ENTRIES {
                        servers.push(server.chars().take(256).collect::<String>());
                    } else {
                        truncated = true;
                    }
                }
            }
            Some("search" | "domain") => {
                for domain in fields {
                    if search_domains.len() < MAX_DNS_ENTRIES {
                        search_domains.push(domain.chars().take(256).collect::<String>());
                    } else {
                        truncated = true;
                    }
                }
            }
            _ => {}
        }
    }
    servers.sort();
    servers.dedup();
    search_domains.sort();
    search_domains.dedup();
    json!({
        "available": true,
        "servers": servers,
        "search_domains": search_domains,
        "truncated": truncated
    })
}

#[cfg(unix)]
pub(super) fn unix_identity(capabilities: Value, umask: Option<String>) -> Value {
    let real_uid = unsafe { libc::getuid() };
    let effective_uid = unsafe { libc::geteuid() };
    let real_gid = unsafe { libc::getgid() };
    let effective_gid = unsafe { libc::getegid() };
    let (groups, groups_available, groups_truncated) = unix_groups();
    json!({
        "real_user": {"id": real_uid, "name": user_name(real_uid)},
        "effective_user": {"id": effective_uid, "name": user_name(effective_uid)},
        "real_group": {"id": real_gid, "name": group_name(real_gid)},
        "effective_group": {"id": effective_gid, "name": group_name(effective_gid)},
        "supplementary_groups": {
            "available": groups_available,
            "items": groups,
            "truncated": groups_truncated
        },
        "is_root": effective_uid == 0,
        "umask": umask,
        "capabilities": capabilities
    })
}

#[cfg(unix)]
fn user_name(uid: libc::uid_t) -> Option<String> {
    let mut record = unsafe { std::mem::zeroed::<libc::passwd>() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; account_buffer_size(libc::_SC_GETPW_R_SIZE_MAX)];
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || record.pw_name.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(record.pw_name) }
            .to_string_lossy()
            .chars()
            .take(256)
            .collect(),
    )
}

#[cfg(unix)]
fn group_name(gid: libc::gid_t) -> Option<String> {
    let mut record = unsafe { std::mem::zeroed::<libc::group>() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; account_buffer_size(libc::_SC_GETGR_R_SIZE_MAX)];
    let rc = unsafe {
        libc::getgrgid_r(
            gid,
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || record.gr_name.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(record.gr_name) }
            .to_string_lossy()
            .chars()
            .take(256)
            .collect(),
    )
}

#[cfg(unix)]
fn account_buffer_size(name: libc::c_int) -> usize {
    let size = unsafe { libc::sysconf(name) };
    if size > 0 {
        usize::try_from(size)
            .unwrap_or(16 * 1024)
            .clamp(1024, 1024 * 1024)
    } else {
        16 * 1024
    }
}

#[cfg(unix)]
fn unix_groups() -> (Vec<Value>, bool, bool) {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return (Vec::new(), false, false);
    }
    let requested = usize::try_from(count).unwrap_or(0);
    if requested > 65_536 {
        return (Vec::new(), false, true);
    }
    let mut ids = vec![0 as libc::gid_t; requested];
    let read = if ids.is_empty() {
        0
    } else {
        unsafe { libc::getgroups(ids.len() as libc::c_int, ids.as_mut_ptr()) }
    };
    if read < 0 {
        return (Vec::new(), false, requested > MAX_GROUPS);
    }
    ids.truncate(usize::try_from(read).unwrap_or(0));
    ids.sort_unstable();
    ids.dedup();
    let truncated = ids.len() > MAX_GROUPS;
    ids.truncate(MAX_GROUPS);
    let groups = ids
        .into_iter()
        .map(|id| json!({"id": id, "name": group_name(id)}))
        .collect();
    (groups, true, truncated)
}

pub(super) fn cpu_snapshot(
    model: Option<String>,
    logical_cores: u64,
    physical_cores: Option<u64>,
    libc_family: &str,
    libc_version: Option<String>,
) -> Value {
    let target = env!("REMOTE_OPS_BUILD_TARGET");
    json!({
        "model": model.map(|value| value.chars().take(512).collect::<String>()),
        "logical_cores": logical_cores,
        "physical_cores": physical_cores,
        "architecture": std::env::consts::ARCH,
        "byte_order": if cfg!(target_endian = "little") { "little" } else { "big" },
        "abi": target.rsplit('-').next().unwrap_or("unknown"),
        "build_target": target,
        "libc": {"family": libc_family, "version": libc_version}
    })
}

pub(super) fn logical_core_count() -> u64 {
    std::thread::available_parallelism()
        .map(|count| u64::try_from(count.get()).unwrap_or(u64::MAX))
        .unwrap_or(1)
}

pub(super) fn time_snapshot(timezone: Option<String>, utc_offset_seconds: Option<i64>) -> Value {
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    json!({
        "unix_seconds": unix_seconds,
        "timezone": timezone.map(|value| value.chars().take(256).collect::<String>()),
        "utc_offset_seconds": utc_offset_seconds
    })
}

#[cfg(unix)]
pub(super) fn unix_time_snapshot() -> Value {
    let timezone = std::env::var("TZ")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            bounded_text("/etc/timezone", 256)
                .map(|(value, _)| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| timezone_from_localtime("/etc/localtime"));
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    let utc_offset_seconds = if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
        None
    } else {
        Some(local.tm_gmtoff as i64)
    };
    time_snapshot(timezone, utc_offset_seconds)
}

#[cfg(unix)]
fn timezone_from_localtime(path: &str) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    let target = target.to_string_lossy();
    let (_, timezone) = target.split_once("zoneinfo/")?;
    (!timezone.is_empty()).then(|| timezone.to_string())
}

pub(super) fn toolchain_snapshot() -> Value {
    const TOOLS: &[&str] = &[
        "cc",
        "gcc",
        "clang",
        "rustc",
        "cargo",
        "go",
        "javac",
        "cmake",
        "make",
        "ninja",
        "gdb",
        "lldb",
        "ld",
        "ar",
        "objcopy",
        "strip",
        "pkg-config",
        "git",
        "python3",
        "python",
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let directories: Vec<_> = std::env::split_paths(&path)
        .take(MAX_PATH_DIRECTORIES + 1)
        .collect();
    let path_truncated = directories.len() > MAX_PATH_DIRECTORIES;
    let directories = &directories[..directories.len().min(MAX_PATH_DIRECTORIES)];
    let items = TOOLS
        .iter()
        .take(MAX_TOOLCHAINS)
        .filter_map(|name| {
            find_program(name, directories).map(|path| {
                json!({
                    "name": name,
                    "path": path.to_string_lossy().chars().take(1024).collect::<String>()
                })
            })
        })
        .collect();
    collection(true, items, path_truncated)
}

fn find_program(name: &str, directories: &[PathBuf]) -> Option<PathBuf> {
    for directory in directories {
        #[cfg(windows)]
        let candidates = windows_program_candidates(directory, name);
        #[cfg(not(windows))]
        let candidates = [directory.join(name)];
        for candidate in candidates {
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_program_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    let extensions = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .take(16)
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".exe".into(), ".cmd".into(), ".bat".into()]);
    let mut candidates = Vec::with_capacity(extensions.len() + 1);
    candidates.push(directory.join(name));
    candidates.extend(
        extensions
            .into_iter()
            .map(|extension| directory.join(format!("{name}{extension}"))),
    );
    candidates
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

#[cfg(windows)]
#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    _memory_load: u32,
    total_physical: u64,
    available_physical: u64,
    _total_page_file: u64,
    _available_page_file: u64,
    _total_virtual: u64,
    _available_virtual: u64,
    _available_extended_virtual: u64,
}

#[cfg(windows)]
#[repr(C)]
struct OsVersionInfoW {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    _platform_id: u32,
    _service_pack: [u16; 128],
}

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn GetComputerNameW(buffer: *mut u16, size: *mut u32) -> i32;
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        available: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> i32;
    fn GetTickCount64() -> u64;
    fn GetWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut OsVersionInfoW) -> i32;
}

#[cfg(windows)]
pub fn system_info() -> AgentResult<Value> {
    let hostname = windows_hostname()?;
    let release = windows_release()?;
    let memory = windows_memory()?;
    let (root_path, total_bytes, available_bytes) = windows_system_filesystem()?;
    let uptime_seconds = unsafe { GetTickCount64() } as f64 / 1000.0;
    let username = std::env::var("USERNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(256).collect::<String>());
    let cpu_model = std::env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .filter(|value| !value.is_empty());
    let filesystem = json!({
        "source": root_path,
        "mount_point": root_path,
        "fs_type": null,
        "total_bytes": total_bytes,
        "available_bytes": available_bytes,
        "total_inodes": null,
        "available_inodes": null,
        "read_only": null
    });
    Ok(json!({
        "hostname": hostname,
        "kernel": {"sysname": "Windows", "release": release, "machine": std::env::consts::ARCH},
        "uptime_seconds": uptime_seconds,
        "load_average": {"one": 0.0, "five": 0.0, "fifteen": 0.0},
        "memory": {"total_bytes": memory.total_physical, "available_bytes": memory.available_physical},
        "root_filesystem": {"total_bytes": total_bytes, "available_bytes": available_bytes},
        "temperatures": [],
        "os": {
            "id": "windows", "id_like": [], "name": "Windows",
            "pretty_name": format!("Windows {release}"), "version": release,
            "version_id": null, "version_codename": null, "variant": null,
            "variant_id": null, "build_id": null, "image_id": null, "image_version": null
        },
        "cpu": cpu_snapshot(cpu_model, logical_core_count(), None, "ucrt", None),
        "identity": {
            "real_user": {"id": null, "name": username},
            "effective_user": {"id": null, "name": username},
            "real_group": {"id": null, "name": null},
            "effective_group": {"id": null, "name": null},
            "supplementary_groups": {"available": false, "items": [], "truncated": false},
            "is_root": null, "umask": null, "capabilities": null
        },
        "network": {
            "interfaces": interface_snapshot(),
            "routes": collection(false, Vec::new(), false),
            "dns": {"available": false, "servers": [], "search_domains": [], "truncated": false},
            "listening_ports": collection(false, Vec::new(), false)
        },
        "filesystems": {"available": true, "mounts": [filesystem], "truncated": false},
        "time": time_snapshot(std::env::var("TZ").ok(), None),
        "init_system": {"name": "windows-service-control-manager", "pid1_comm": null},
        "toolchains": toolchain_snapshot()
    }))
}

#[cfg(windows)]
fn windows_hostname() -> AgentResult<String> {
    let mut buffer = [0u16; 256];
    let mut size = buffer.len() as u32;
    if unsafe { GetComputerNameW(buffer.as_mut_ptr(), &mut size) } == 0 {
        return Err(AgentError::io(
            "GetComputerNameW",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(String::from_utf16_lossy(&buffer[..size as usize]))
}

#[cfg(windows)]
fn windows_release() -> AgentResult<String> {
    let mut version = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        _platform_id: 0,
        _service_pack: [0; 128],
    };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status != 0 {
        return Err(AgentError::command(format!(
            "RtlGetVersion failed with NTSTATUS 0x{:08x}",
            status as u32
        )));
    }
    Ok(format!(
        "{}.{}.{}",
        version.major, version.minor, version.build
    ))
}

#[cfg(windows)]
fn windows_memory() -> AgentResult<MemoryStatusEx> {
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        _memory_load: 0,
        total_physical: 0,
        available_physical: 0,
        _total_page_file: 0,
        _available_page_file: 0,
        _total_virtual: 0,
        _available_virtual: 0,
        _available_extended_virtual: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(AgentError::io(
            "GlobalMemoryStatusEx",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(status)
}

#[cfg(windows)]
fn windows_system_filesystem() -> AgentResult<(String, u64, u64)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let mut buffer = vec![0u16; 260];
    let windows_directory = loop {
        let written = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if written == 0 {
            return Err(AgentError::io(
                "GetWindowsDirectoryW",
                std::io::Error::last_os_error(),
            ));
        }
        if (written as usize) < buffer.len() {
            break PathBuf::from(OsString::from_wide(&buffer[..written as usize]));
        }
        buffer.resize(written as usize + 1, 0);
    };
    let root = windows_directory
        .ancestors()
        .filter(|path| path.has_root())
        .last()
        .ok_or_else(|| AgentError::command("Windows directory has no filesystem root"))?;
    let root_wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    let mut total = 0u64;
    if unsafe {
        GetDiskFreeSpaceExW(
            root_wide.as_ptr(),
            &mut available,
            &mut total,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(AgentError::io(
            format!("GetDiskFreeSpaceExW {}", root.display()),
            std::io::Error::last_os_error(),
        ));
    }
    Ok((root.to_string_lossy().into_owned(), total, available))
}

#[cfg(target_os = "macos")]
pub fn system_info() -> AgentResult<Value> {
    let hostname = macos_hostname()?;
    let kernel = macos_kernel()?;
    let uptime_seconds = macos_uptime();
    let loads = macos_load_average();
    let (total_memory, available_memory) = macos_memory()?;
    let filesystem = macos_root_filesystem()?;
    let version = macos_sysctl_string("kern.osproductversion");
    let cpu_model =
        macos_sysctl_string("machdep.cpu.brand_string").or_else(|| macos_sysctl_string("hw.model"));
    let logical_cores = macos_sysctl_u64("hw.logicalcpu").unwrap_or_else(logical_core_count);
    let physical_cores = macos_sysctl_u64("hw.physicalcpu");
    Ok(json!({
        "hostname": hostname,
        "kernel": kernel,
        "uptime_seconds": uptime_seconds,
        "load_average": {"one": loads[0], "five": loads[1], "fifteen": loads[2]},
        "memory": {"total_bytes": total_memory, "available_bytes": available_memory},
        "root_filesystem": {"total_bytes": filesystem.0, "available_bytes": filesystem.1},
        "temperatures": [],
        "os": {
            "id": "macos", "id_like": [], "name": "macOS", "pretty_name": "macOS",
            "version": version, "version_id": version, "version_codename": null,
            "variant": null, "variant_id": null, "build_id": null,
            "image_id": null, "image_version": null
        },
        "cpu": cpu_snapshot(cpu_model, logical_cores, physical_cores, "libSystem", None),
        "identity": unix_identity(Value::Null, None),
        "network": {
            "interfaces": interface_snapshot(),
            "routes": collection(false, Vec::new(), false),
            "dns": resolv_conf_snapshot(),
            "listening_ports": collection(false, Vec::new(), false)
        },
        "filesystems": {"available": true, "mounts": [{
            "source": "/", "mount_point": "/", "fs_type": null,
            "total_bytes": filesystem.0, "available_bytes": filesystem.1,
            "total_inodes": filesystem.2, "available_inodes": filesystem.3,
            "read_only": filesystem.4
        }], "truncated": false},
        "time": unix_time_snapshot(),
        "init_system": {"name": "launchd", "pid1_comm": "launchd"},
        "toolchains": toolchain_snapshot()
    }))
}

#[cfg(target_os = "macos")]
fn macos_hostname() -> AgentResult<String> {
    use std::ffi::CStr;
    let mut buffer = [0 as libc::c_char; 256];
    if unsafe { libc::gethostname(buffer.as_mut_ptr(), buffer.len()) } != 0 {
        return Err(AgentError::io(
            "gethostname",
            std::io::Error::last_os_error(),
        ));
    }
    buffer[buffer.len() - 1] = 0;
    Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned())
}

#[cfg(target_os = "macos")]
fn macos_kernel() -> AgentResult<Value> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    let mut uts = MaybeUninit::<libc::utsname>::uninit();
    if unsafe { libc::uname(uts.as_mut_ptr()) } != 0 {
        return Err(AgentError::io("uname", std::io::Error::last_os_error()));
    }
    let uts = unsafe { uts.assume_init() };
    let cstr = |field: &[libc::c_char]| {
        unsafe { CStr::from_ptr(field.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    Ok(json!({
        "sysname": cstr(&uts.sysname),
        "release": cstr(&uts.release),
        "machine": cstr(&uts.machine),
    }))
}

#[cfg(target_os = "macos")]
fn macos_uptime() -> f64 {
    use std::mem::MaybeUninit;
    let name = std::ffi::CString::new("kern.boottime").expect("literal");
    let mut boot = MaybeUninit::<libc::timeval>::uninit();
    let mut size = std::mem::size_of::<libc::timeval>();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            boot.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return 0.0;
    }
    let boot = unsafe { boot.assume_init() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    (now - boot.tv_sec).max(0) as f64
}

#[cfg(target_os = "macos")]
fn macos_load_average() -> [f64; 3] {
    let mut loads = [0f64; 3];
    let count = unsafe { libc::getloadavg(loads.as_mut_ptr(), loads.len() as i32) };
    if count < 1 {
        return [0.0; 3];
    }
    [
        loads[0],
        if count > 1 { loads[1] } else { 0.0 },
        if count > 2 { loads[2] } else { 0.0 },
    ]
}

#[cfg(target_os = "macos")]
fn macos_memory() -> AgentResult<(u64, u64)> {
    Ok((
        macos_sysctl_u64("hw.memsize").unwrap_or(0),
        macos_available_memory()?,
    ))
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn macos_available_memory() -> AgentResult<u64> {
    let mut vm: libc::vm_statistics64_data_t = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let result = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            (&mut vm as *mut libc::vm_statistics64_data_t).cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return Err(AgentError::command(format!(
            "host_statistics failed with kern_return_t {result}"
        )));
    }
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = if page_size > 0 {
        page_size as u64
    } else {
        4096
    };
    let reclaimable = vm.free_count as u64
        + vm.inactive_count as u64
        + vm.purgeable_count as u64
        + vm.speculative_count as u64;
    Ok(reclaimable.saturating_mul(page_size))
}

#[cfg(target_os = "macos")]
fn macos_root_filesystem() -> AgentResult<(u64, u64, u64, u64, bool)> {
    use std::mem::MaybeUninit;
    let root = std::ffi::CString::new("/").expect("literal");
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(root.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(AgentError::io("statvfs /", std::io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    Ok((
        (stat.f_blocks as u64).saturating_mul(stat.f_frsize),
        (stat.f_bavail as u64).saturating_mul(stat.f_frsize),
        stat.f_files as u64,
        stat.f_favail as u64,
        stat.f_flag & libc::ST_RDONLY != 0,
    ))
}

#[cfg(target_os = "macos")]
fn macos_sysctl_string(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut size = 0usize;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > 4096
    {
        return None;
    }
    let mut buffer = vec![0u8; size];
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    buffer.truncate(size);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

#[cfg(target_os = "macos")]
fn macos_sysctl_u64(name: &str) -> Option<u64> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = 0u64;
    let mut size = std::mem::size_of::<u64>();
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } == 0
    {
        Some(value)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn system_info() -> AgentResult<Value> {
    Err(AgentError::unsupported(
        "system_info requires Linux, macOS or Windows",
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::system_info;

    #[test]
    fn windows_system_info_reports_real_resources_and_extended_shape() {
        let result = system_info().unwrap();
        assert_eq!(result["kernel"]["sysname"], "Windows");
        assert!(!result["hostname"].as_str().unwrap().is_empty());
        assert!(!result["kernel"]["release"].as_str().unwrap().is_empty());
        assert!(result["uptime_seconds"].as_f64().unwrap() >= 0.0);
        assert_eq!(result["os"]["id"], "windows");
        assert!(result["cpu"]["logical_cores"].as_u64().unwrap() > 0);
        assert!(result["network"]["interfaces"]["items"].is_array());
        assert_eq!(result["filesystems"]["mounts"].as_array().unwrap().len(), 1);
        assert!(result["time"]["unix_seconds"].as_u64().unwrap() > 0);
        assert!(result["toolchains"]["items"].is_array());

        let total_memory = result["memory"]["total_bytes"].as_u64().unwrap();
        let available_memory = result["memory"]["available_bytes"].as_u64().unwrap();
        assert!(total_memory > 0);
        assert!(available_memory <= total_memory);

        let total_disk = result["root_filesystem"]["total_bytes"].as_u64().unwrap();
        let available_disk = result["root_filesystem"]["available_bytes"]
            .as_u64()
            .unwrap();
        assert!(total_disk > 0);
        assert!(available_disk <= total_disk);
        assert!(result["temperatures"].as_array().unwrap().is_empty());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::system_info;

    #[test]
    fn macos_system_info_reports_real_resources_and_extended_shape() {
        let result = system_info().unwrap();
        assert_eq!(result["kernel"]["sysname"], "Darwin");
        assert!(!result["hostname"].as_str().unwrap().is_empty());
        assert!(!result["kernel"]["release"].as_str().unwrap().is_empty());
        assert!(result["uptime_seconds"].as_f64().unwrap() >= 0.0);
        assert_eq!(result["os"]["id"], "macos");
        assert!(result["cpu"]["logical_cores"].as_u64().unwrap() > 0);
        assert!(result["identity"]["effective_user"]["id"].is_u64());
        assert!(result["network"]["interfaces"]["items"].is_array());
        assert_eq!(result["filesystems"]["mounts"].as_array().unwrap().len(), 1);

        let total_memory = result["memory"]["total_bytes"].as_u64().unwrap();
        let available_memory = result["memory"]["available_bytes"].as_u64().unwrap();
        assert!(total_memory > 0);
        assert!(available_memory <= total_memory);
        assert!(result["temperatures"].as_array().unwrap().is_empty());
    }
}
