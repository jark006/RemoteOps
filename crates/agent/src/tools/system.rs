use serde_json::Value;
#[cfg(any(target_os = "linux", windows))]
use serde_json::json;

use crate::error::{AgentError, AgentResult};

#[cfg(target_os = "linux")]
pub fn system_info() -> AgentResult<Value> {
    use std::ffi::CStr;
    use std::fs;
    use std::mem::MaybeUninit;
    let mut hostname_buffer = [0 as libc::c_char; 256];
    if unsafe { libc::gethostname(hostname_buffer.as_mut_ptr(), hostname_buffer.len()) } != 0 {
        return Err(AgentError::io(
            "gethostname",
            std::io::Error::last_os_error(),
        ));
    }
    hostname_buffer[hostname_buffer.len() - 1] = 0;
    let hostname = unsafe { CStr::from_ptr(hostname_buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
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
    let uptime = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    let loads: Vec<f64> = fs::read_to_string("/proc/loadavg")
        .unwrap_or_default()
        .split_whitespace()
        .take(3)
        .filter_map(|v| v.parse().ok())
        .collect();
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mem = |key: &str| {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|v| v * 1024)
    };
    let root = std::ffi::CString::new("/").expect("literal");
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(root.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(AgentError::io("statvfs /", std::io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    let temperatures = temperatures();
    Ok(json!({
        "hostname": hostname,
        "kernel": {"sysname": cstr(&uts.sysname), "release": cstr(&uts.release), "machine": cstr(&uts.machine)},
        "uptime_seconds": uptime,
        "load_average": {"one": loads.first().copied().unwrap_or(0.0), "five": loads.get(1).copied().unwrap_or(0.0), "fifteen": loads.get(2).copied().unwrap_or(0.0)},
        "memory": {"total_bytes": mem("MemTotal").unwrap_or(0), "available_bytes": mem("MemAvailable").unwrap_or(0)},
        "root_filesystem": {"total_bytes": (stat.f_blocks as u64).saturating_mul(stat.f_frsize as u64), "available_bytes": (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)},
        "temperatures": temperatures
    }))
}

#[cfg(target_os = "linux")]
fn temperatures() -> Vec<Value> {
    use std::fs;
    let Ok(entries) = fs::read_dir("/sys/class/thermal") else {
        return Vec::new();
    };
    entries
        .flatten()
        .take(64)
        .filter_map(|entry| {
            let path = entry.path();
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("thermal_zone")
            {
                return None;
            }
            let raw = fs::read_to_string(path.join("temp"))
                .ok()?
                .trim()
                .parse::<f64>()
                .ok()?;
            let name = fs::read_to_string(path.join("type"))
                .ok()?
                .trim()
                .to_string();
            Some(json!({"name": name, "celsius": raw / 1000.0}))
        })
        .collect()
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
    let filesystem = windows_system_filesystem()?;
    let uptime_seconds = unsafe { GetTickCount64() } as f64 / 1000.0;

    Ok(json!({
        "hostname": hostname,
        "kernel": {"sysname": "Windows", "release": release, "machine": std::env::consts::ARCH},
        "uptime_seconds": uptime_seconds,
        "load_average": {"one": 0.0, "five": 0.0, "fifteen": 0.0},
        "memory": {"total_bytes": memory.total_physical, "available_bytes": memory.available_physical},
        "root_filesystem": {"total_bytes": filesystem.0, "available_bytes": filesystem.1},
        "temperatures": []
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
fn windows_system_filesystem() -> AgentResult<(u64, u64)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

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
    Ok((total, available))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn system_info() -> AgentResult<Value> {
    Err(AgentError::unsupported(
        "system_info requires Linux or Windows",
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::system_info;

    #[test]
    fn windows_system_info_reports_real_resources() {
        let result = system_info().unwrap();
        assert_eq!(result["kernel"]["sysname"], "Windows");
        assert!(!result["hostname"].as_str().unwrap().is_empty());
        assert!(!result["kernel"]["release"].as_str().unwrap().is_empty());
        assert!(result["uptime_seconds"].as_f64().unwrap() >= 0.0);

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
