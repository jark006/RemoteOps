use serde_json::Value;
#[cfg(any(target_os = "linux", unix, windows))]
use serde_json::json;

use crate::error::{AgentError, AgentResult};

#[cfg(target_os = "linux")]
pub fn pids(filter: Option<&str>, cursor: Option<&str>, limit: usize) -> AgentResult<Value> {
    use std::fs;
    if limit == 0 || limit > 1024 {
        return Err(AgentError::invalid("limit must be in range 1..=1024"));
    }
    let mut ids: Vec<u32> = fs::read_dir("/proc")
        .map_err(|err| AgentError::io("list /proc", err))?
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
        .collect();
    ids.sort_unstable();
    let after_pid = match cursor {
        Some(value) => value
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| AgentError::invalid("cursor must be a positive PID"))?,
        None => 0,
    };
    let mut processes = Vec::new();
    let mut more = false;
    for pid in ids.into_iter().filter(|pid| *pid > after_pid) {
        let name = fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .replace('\0', " ")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        if filter.is_some_and(|needle| !name.contains(needle) && !cmdline.contains(needle)) {
            continue;
        }
        if processes.len() == limit {
            more = true;
            break;
        }
        processes.push(json!({"pid": pid, "name": name, "cmdline": cmdline}));
    }
    let next_cursor = if more {
        processes
            .last()
            .and_then(|value| value["pid"].as_u64())
            .map(|pid| pid.to_string())
    } else {
        None
    };
    Ok(json!({"processes": processes, "next_cursor": next_cursor, "truncated": more}))
}

#[cfg(windows)]
pub fn pids(filter: Option<&str>, cursor: Option<&str>, limit: usize) -> AgentResult<Value> {
    if limit == 0 || limit > 1024 {
        return Err(AgentError::invalid("limit must be in range 1..=1024"));
    }
    let after_pid = parse_cursor(cursor)?;
    let mut entries = windows_process_entries()?;
    entries.sort_unstable_by_key(|entry| entry.pid);

    let mut processes = Vec::new();
    let mut more = false;
    for entry in entries.into_iter().filter(|entry| entry.pid > after_pid) {
        let cmdline = windows_process_command_line(entry.pid).unwrap_or_default();
        if filter.is_some_and(|needle| !entry.name.contains(needle) && !cmdline.contains(needle)) {
            continue;
        }
        if processes.len() == limit {
            more = true;
            break;
        }
        processes.push(json!({"pid": entry.pid, "name": entry.name, "cmdline": cmdline}));
    }
    let next_cursor = if more {
        processes
            .last()
            .and_then(|value| value["pid"].as_u64())
            .map(|pid| pid.to_string())
    } else {
        None
    };
    Ok(json!({"processes": processes, "next_cursor": next_cursor, "truncated": more}))
}

#[cfg(target_os = "macos")]
pub fn pids(filter: Option<&str>, cursor: Option<&str>, limit: usize) -> AgentResult<Value> {
    if limit == 0 || limit > 1024 {
        return Err(AgentError::invalid("limit must be in range 1..=1024"));
    }
    let after_pid = parse_cursor(cursor)?;
    let ids = macos_process_ids()?;

    let mut processes = Vec::new();
    let mut more = false;
    for pid in ids.into_iter().filter(|pid| *pid > after_pid) {
        let Ok(info) = macos_bsd_info(pid as i32) else {
            continue;
        };
        let name = macos_process_name(&info);
        let cmdline = macos_process_command_line(pid as i32).unwrap_or_default();
        if filter.is_some_and(|needle| !name.contains(needle) && !cmdline.contains(needle)) {
            continue;
        }
        if processes.len() == limit {
            more = true;
            break;
        }
        processes.push(json!({"pid": pid, "name": name, "cmdline": cmdline}));
    }
    let next_cursor = if more {
        processes
            .last()
            .and_then(|value| value["pid"].as_u64())
            .map(|pid| pid.to_string())
    } else {
        None
    };
    Ok(json!({"processes": processes, "next_cursor": next_cursor, "truncated": more}))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn pids(_filter: Option<&str>, _cursor: Option<&str>, _limit: usize) -> AgentResult<Value> {
    Err(AgentError::unsupported(
        "pids requires Linux, macOS, or Windows",
    ))
}

#[cfg(target_os = "linux")]
pub fn process_info(pid: i32) -> AgentResult<Value> {
    use std::fs;
    if pid <= 0 {
        return Err(AgentError::invalid("pid must be greater than zero"));
    }
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|err| AgentError::io(format!("read process {pid}"), err))?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|err| AgentError::io(format!("read process stat {pid}"), err))?;
    let value = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .map(str::trim)
            .unwrap_or("")
    };
    let parse_first = |key: &str| {
        value(key)
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
    };
    let name = value("Name");
    if name.is_empty() {
        return Err(AgentError::command("process status is missing Name"));
    }
    let state = value("State");
    if state.is_empty() {
        return Err(AgentError::command("process status is missing State"));
    }
    let ppid =
        parse_first("PPid").ok_or_else(|| AgentError::command("process status PPid is invalid"))?;
    let uid =
        parse_first("Uid").ok_or_else(|| AgentError::command("process status Uid is invalid"))?;
    let rss_bytes = parse_first("VmRSS").map(|v| v * 1024);
    let virtual_memory_bytes = parse_first("VmSize").map(|v| v * 1024);
    let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let close = stat
        .rfind(')')
        .ok_or_else(|| AgentError::command("malformed /proc stat"))?;
    let start_time_ticks = stat[close + 1..]
        .split_whitespace()
        .nth(19)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| AgentError::command("process stat is missing start time"))?;
    let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clock_ticks <= 0 {
        return Err(AgentError::command("could not read system clock ticks"));
    }
    Ok(
        json!({"pid": pid, "ppid": ppid, "name": name, "state": state, "cmdline": cmdline, "uid": uid, "resident_bytes": rss_bytes, "virtual_bytes": virtual_memory_bytes, "start_time_ticks": start_time_ticks, "start_time_seconds": start_time_ticks as f64 / clock_ticks as f64}),
    )
}

#[cfg(windows)]
pub fn process_info(pid: i32) -> AgentResult<Value> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount64};
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    if pid <= 0 {
        return Err(AgentError::invalid("pid must be greater than zero"));
    }
    let pid = pid as u32;
    let entry = windows_process_entries()?
        .into_iter()
        .find(|entry| entry.pid == pid)
        .ok_or_else(|| AgentError::io(format!("read process {pid}"), not_found_error()))?;
    let process = open_process_for_query(pid)?;
    let cmdline = query_process_command_line(process.raw()).unwrap_or_default();

    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { zeroed() };
    memory.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    if unsafe { K32GetProcessMemoryInfo(process.raw(), &mut memory, memory.cb) } == 0 {
        return Err(AgentError::io(
            format!("read process memory {pid}"),
            std::io::Error::last_os_error(),
        ));
    }
    let virtual_bytes = process_virtual_bytes(process.raw());

    let mut creation: FILETIME = unsafe { zeroed() };
    let mut exit: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    if unsafe {
        GetProcessTimes(
            process.raw(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(AgentError::io(
            format!("read process times {pid}"),
            std::io::Error::last_os_error(),
        ));
    }
    let creation_ticks = filetime_ticks(creation);
    let mut now: FILETIME = unsafe { zeroed() };
    unsafe { GetSystemTimeAsFileTime(&mut now) };
    let uptime_ticks = unsafe { GetTickCount64() }.saturating_mul(10_000);
    let boot_ticks = filetime_ticks(now).saturating_sub(uptime_ticks);
    let start_time_ticks = creation_ticks.saturating_sub(boot_ticks).min(uptime_ticks);

    Ok(json!({
        "pid": pid,
        "ppid": entry.ppid,
        "name": entry.name,
        "state": Value::Null,
        "cmdline": cmdline,
        "uid": Value::Null,
        "resident_bytes": memory.WorkingSetSize as u64,
        "virtual_bytes": virtual_bytes,
        "start_time_ticks": start_time_ticks,
        "start_time_seconds": start_time_ticks as f64 / 10_000_000.0
    }))
}

#[cfg(target_os = "macos")]
pub fn process_info(pid: i32) -> AgentResult<Value> {
    if pid <= 0 {
        return Err(AgentError::invalid("pid must be greater than zero"));
    }
    let info = macos_bsd_info(pid)?;
    let task = macos_task_info(pid)?;
    let start_time_ticks = macos_process_start_ticks(&info)?;

    Ok(json!({
        "pid": pid,
        "ppid": info.pbi_ppid,
        "name": macos_process_name(&info),
        "state": macos_process_state(info.pbi_status),
        "cmdline": macos_process_command_line(pid).unwrap_or_default(),
        "uid": info.pbi_uid,
        "resident_bytes": task.pti_resident_size,
        "virtual_bytes": task.pti_virtual_size,
        "start_time_ticks": start_time_ticks,
        "start_time_seconds": start_time_ticks as f64 / 1_000_000.0
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn process_info(_pid: i32) -> AgentResult<Value> {
    Err(AgentError::unsupported(
        "process_info requires Linux, macOS, or Windows",
    ))
}

#[cfg(target_os = "macos")]
const PROC_PIDTBSDINFO: libc::c_int = 3;
#[cfg(target_os = "macos")]
const PROC_PIDTASKINFO: libc::c_int = 4;
#[cfg(target_os = "macos")]
const CTL_KERN: libc::c_int = 1;
#[cfg(target_os = "macos")]
const KERN_PROCARGS2: libc::c_int = 49;
#[cfg(target_os = "macos")]
const MAX_COMMAND_LINE_BYTES: usize = 128 * 1024;
#[cfg(target_os = "macos")]
const MAX_PROCARGS_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [libc::c_char; 16],
    pbi_name: [libc::c_char; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosTaskInfo {
    pti_virtual_size: u64,
    pti_resident_size: u64,
    pti_total_user: u64,
    pti_total_system: u64,
    pti_threads_user: u64,
    pti_threads_system: u64,
    pti_policy: i32,
    pti_faults: i32,
    pti_pageins: i32,
    pti_cow_faults: i32,
    pti_messages_sent: i32,
    pti_messages_received: i32,
    pti_syscalls_mach: i32,
    pti_syscalls_unix: i32,
    pti_csw: i32,
    pti_threadnum: i32,
    pti_numrunning: i32,
    pti_priority: i32,
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        arg: u64,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn macos_process_ids() -> AgentResult<Vec<u32>> {
    use std::mem::size_of;

    let estimated = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if estimated <= 0 {
        return Err(AgentError::io(
            "list processes",
            std::io::Error::last_os_error(),
        ));
    }
    let capacity = (estimated as usize).saturating_add(64);
    let buffer_bytes = capacity
        .checked_mul(size_of::<libc::pid_t>())
        .and_then(|value| libc::c_int::try_from(value).ok())
        .ok_or_else(|| AgentError::command("process list buffer is too large"))?;
    let mut buffer = vec![0 as libc::pid_t; capacity];
    let count = unsafe { proc_listallpids(buffer.as_mut_ptr().cast(), buffer_bytes) };
    if count < 0 {
        return Err(AgentError::io(
            "list processes",
            std::io::Error::last_os_error(),
        ));
    }
    buffer.truncate((count as usize).min(buffer.len()));
    let mut ids: Vec<u32> = buffer
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

#[cfg(target_os = "macos")]
fn macos_bsd_info(pid: i32) -> AgentResult<MacosBsdInfo> {
    use std::mem::{size_of, zeroed};

    let mut info: MacosBsdInfo = unsafe { zeroed() };
    let expected = size_of::<MacosBsdInfo>();
    let read = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut MacosBsdInfo).cast(),
            expected as libc::c_int,
        )
    };
    if read != expected as libc::c_int {
        Err(AgentError::io(
            format!("read process {pid}"),
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(info)
    }
}

#[cfg(target_os = "macos")]
fn macos_task_info(pid: i32) -> AgentResult<MacosTaskInfo> {
    use std::mem::{size_of, zeroed};

    let mut info: MacosTaskInfo = unsafe { zeroed() };
    let expected = size_of::<MacosTaskInfo>();
    let read = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTASKINFO,
            0,
            (&mut info as *mut MacosTaskInfo).cast(),
            expected as libc::c_int,
        )
    };
    if read != expected as libc::c_int {
        Err(AgentError::io(
            format!("read process memory {pid}"),
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(info)
    }
}

#[cfg(target_os = "macos")]
fn macos_process_name(info: &MacosBsdInfo) -> String {
    let name = macos_c_string(&info.pbi_name);
    if name.is_empty() {
        macos_c_string(&info.pbi_comm)
    } else {
        name
    }
}

#[cfg(target_os = "macos")]
fn macos_c_string(value: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = value
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "macos")]
fn macos_process_state(status: u32) -> &'static str {
    match status {
        1 => "idle",
        2 => "running",
        3 => "sleeping",
        4 => "stopped",
        5 => "zombie",
        _ => "unknown",
    }
}

#[cfg(target_os = "macos")]
fn macos_process_command_line(pid: i32) -> AgentResult<String> {
    use std::ffi::c_void;
    use std::mem::size_of;

    let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
    let mut required = 0usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut required,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(AgentError::io(
            format!("read process command line {pid}"),
            std::io::Error::last_os_error(),
        ));
    }
    if required < size_of::<libc::c_int>() || required > MAX_PROCARGS_BUFFER_BYTES {
        return Err(AgentError::command(
            "process command line buffer length is invalid",
        ));
    }

    let mut buffer = vec![0u8; required];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut required,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(AgentError::io(
            format!("read process command line {pid}"),
            std::io::Error::last_os_error(),
        ));
    }
    buffer.truncate(required);
    macos_parse_command_line(&buffer)
}

#[cfg(target_os = "macos")]
fn macos_parse_command_line(buffer: &[u8]) -> AgentResult<String> {
    use std::mem::size_of;

    if buffer.len() < size_of::<libc::c_int>() {
        return Err(AgentError::command("process command line is truncated"));
    }
    let argc = libc::c_int::from_ne_bytes(
        buffer[..size_of::<libc::c_int>()]
            .try_into()
            .expect("c_int size is four bytes on macOS"),
    );
    if argc <= 0 {
        return Ok(String::new());
    }

    let mut offset = size_of::<libc::c_int>();
    while offset < buffer.len() && buffer[offset] != 0 {
        offset += 1;
    }
    while offset < buffer.len() && buffer[offset] == 0 {
        offset += 1;
    }

    let mut arguments = Vec::new();
    for _ in 0..argc {
        if offset >= buffer.len() {
            break;
        }
        let end = buffer[offset..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| offset + length)
            .unwrap_or(buffer.len());
        arguments.push(String::from_utf8_lossy(&buffer[offset..end]).into_owned());
        offset = end.saturating_add(1);
    }
    let mut command_line = arguments.join(" ");
    if command_line.len() > MAX_COMMAND_LINE_BYTES {
        let mut boundary = MAX_COMMAND_LINE_BYTES;
        while !command_line.is_char_boundary(boundary) {
            boundary -= 1;
        }
        command_line.truncate(boundary);
    }
    Ok(command_line)
}

#[cfg(target_os = "macos")]
fn macos_process_start_ticks(info: &MacosBsdInfo) -> AgentResult<u64> {
    use std::mem::{size_of, zeroed};

    let mut boot_time: libc::timeval = unsafe { zeroed() };
    let mut size = size_of::<libc::timeval>();
    let name = c"kern.boottime";
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut boot_time as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size != size_of::<libc::timeval>()
    {
        return Err(AgentError::io(
            "read system boot time",
            std::io::Error::last_os_error(),
        ));
    }

    let boot_seconds = u64::try_from(boot_time.tv_sec)
        .map_err(|_| AgentError::command("system boot time is invalid"))?;
    let boot_micros = boot_seconds
        .saturating_mul(1_000_000)
        .saturating_add(boot_time.tv_usec as u64);
    let start_micros = info
        .pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec);
    Ok(start_micros.saturating_sub(boot_micros))
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsProcessEntry {
    pid: u32,
    ppid: u32,
    name: String,
}

#[cfg(any(target_os = "macos", windows))]
fn parse_cursor(cursor: Option<&str>) -> AgentResult<u32> {
    match cursor {
        Some(value) => value
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| AgentError::invalid("cursor must be a positive PID")),
        None => Ok(0),
    }
}

#[cfg(windows)]
fn windows_process_entries() -> AgentResult<Vec<WindowsProcessEntry>> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(AgentError::io(
            "create process snapshot",
            std::io::Error::last_os_error(),
        ));
    }
    let snapshot = OwnedHandle(snapshot);
    let mut raw: PROCESSENTRY32W = unsafe { zeroed() };
    raw.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    if unsafe { Process32FirstW(snapshot.raw(), &mut raw) } == 0 {
        return Err(AgentError::io(
            "read process snapshot",
            std::io::Error::last_os_error(),
        ));
    }

    let mut entries = Vec::new();
    loop {
        if raw.th32ProcessID > 0 {
            let name_len = raw
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(raw.szExeFile.len());
            entries.push(WindowsProcessEntry {
                pid: raw.th32ProcessID,
                ppid: raw.th32ParentProcessID,
                name: String::from_utf16_lossy(&raw.szExeFile[..name_len]),
            });
        }
        if unsafe { Process32NextW(snapshot.raw(), &mut raw) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(AgentError::io("read process snapshot", error));
        }
    }
    Ok(entries)
}

#[cfg(windows)]
fn windows_process_command_line(pid: u32) -> Option<String> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let process = OwnedHandle(handle);
    query_process_command_line(process.raw()).ok()
}

#[cfg(windows)]
fn open_process_for_query(pid: u32) -> AgentResult<OwnedHandle> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        Err(AgentError::io(
            format!("open process {pid}"),
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(OwnedHandle(handle))
    }
}

#[cfg(windows)]
fn process_virtual_bytes(process: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::Memory::{MEM_FREE, MEMORY_BASIC_INFORMATION, VirtualQueryEx};

    let mut address = 0usize;
    let mut total = 0u64;
    loop {
        let mut region: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
        if unsafe {
            VirtualQueryEx(
                process,
                address as *const c_void,
                &mut region,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        } == 0
        {
            break;
        }
        if region.State != MEM_FREE {
            total = total.saturating_add(region.RegionSize as u64);
        }
        let Some(next) = (region.BaseAddress as usize).checked_add(region.RegionSize) else {
            break;
        };
        if next <= address {
            break;
        }
        address = next;
    }
    total
}

#[cfg(windows)]
fn query_process_command_line(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> AgentResult<String> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::UNICODE_STRING;

    const MAX_COMMAND_LINE_BYTES: u32 = 128 * 1024;
    let mut required = 0u32;
    unsafe {
        NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    };
    if required < size_of::<UNICODE_STRING>() as u32 || required > MAX_COMMAND_LINE_BYTES {
        return Err(AgentError::command(
            "process command line length is invalid",
        ));
    }

    let word_count = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; word_count];
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            buffer.as_mut_ptr().cast::<c_void>(),
            required,
            &mut required,
        )
    };
    if status < 0 {
        return Err(AgentError::command(format!(
            "NtQueryInformationProcess failed with NTSTATUS 0x{:08x}",
            status as u32
        )));
    }

    let value = unsafe { &*buffer.as_ptr().cast::<UNICODE_STRING>() };
    let byte_len = value.Length as usize;
    let start = buffer.as_ptr() as usize;
    let end = start + buffer.len() * size_of::<usize>();
    let text_start = value.Buffer as usize;
    let text_end = text_start
        .checked_add(byte_len)
        .ok_or_else(|| AgentError::command("process command line pointer overflow"))?;
    if !byte_len.is_multiple_of(2) || text_start < start || text_end > end {
        return Err(AgentError::command(
            "process command line buffer is invalid",
        ));
    }
    let wide = unsafe { std::slice::from_raw_parts(value.Buffer, byte_len / 2) };
    Ok(String::from_utf16_lossy(wide))
}

#[cfg(windows)]
fn filetime_ticks(value: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

#[cfg(windows)]
fn not_found_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(windows_sys::Win32::Foundation::ERROR_NOT_FOUND as i32)
}

#[cfg(windows)]
struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(unix)]
pub fn kill(pid: i32, signal: i32) -> AgentResult<Value> {
    if pid <= 0 {
        return Err(AgentError::invalid("pid must be greater than zero"));
    }
    if !(1..=64).contains(&signal) {
        return Err(AgentError::invalid("signal must be in range 1..=64"));
    }
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(json!({"pid": pid, "signal": signal}))
    } else {
        Err(AgentError::io(
            format!("kill process {pid}"),
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(windows)]
pub fn kill(pid: i32, signal: i32) -> AgentResult<Value> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    if pid <= 0 {
        return Err(AgentError::invalid("pid must be greater than zero"));
    }
    if !matches!(signal, 9 | 15) {
        return Err(AgentError::invalid("signal must be 9 or 15 on Windows"));
    }

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
    if handle.is_null() {
        return Err(AgentError::io(
            format!("open process {pid} for termination"),
            std::io::Error::last_os_error(),
        ));
    }
    let process = OwnedHandle(handle);
    if unsafe { TerminateProcess(process.raw(), 1) } == 0 {
        return Err(AgentError::io(
            format!("terminate process {pid}"),
            std::io::Error::last_os_error(),
        ));
    }

    Ok(json!({"pid": pid, "signal": signal}))
}

#[cfg(not(any(unix, windows)))]
pub fn kill(_pid: i32, _signal: i32) -> AgentResult<Value> {
    Err(AgentError::unsupported("kill requires Unix or Windows"))
}

#[cfg(target_os = "linux")]
const LINUX_PKILL_MAX_NAME_BYTES: usize = 15;
#[cfg(target_os = "macos")]
const MACOS_PKILL_MAX_NAME_BYTES: usize = 31;
#[cfg(windows)]
const WINDOWS_PKILL_MAX_NAME_UNITS: usize = 260;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
const PKILL_MAX_TARGETS: usize = 1024;

fn validate_pkill_args(name: &str, signal: i32) -> AgentResult<()> {
    if name.is_empty() {
        return Err(AgentError::invalid("name must not be empty"));
    }
    if name.contains('\0') {
        return Err(AgentError::invalid("name must not contain NUL"));
    }
    #[cfg(target_os = "linux")]
    if name.len() > LINUX_PKILL_MAX_NAME_BYTES {
        return Err(AgentError::invalid(format!(
            "name must not exceed {LINUX_PKILL_MAX_NAME_BYTES} bytes on Linux"
        )));
    }
    #[cfg(target_os = "macos")]
    if name.len() > MACOS_PKILL_MAX_NAME_BYTES {
        return Err(AgentError::invalid(format!(
            "name must not exceed {MACOS_PKILL_MAX_NAME_BYTES} bytes on macOS"
        )));
    }
    #[cfg(windows)]
    if name.encode_utf16().count() > WINDOWS_PKILL_MAX_NAME_UNITS {
        return Err(AgentError::invalid(format!(
            "name must not exceed {WINDOWS_PKILL_MAX_NAME_UNITS} UTF-16 code units on Windows"
        )));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    if name.chars().count() > 260 {
        return Err(AgentError::invalid("name must not exceed 260 characters"));
    }
    #[cfg(windows)]
    if !matches!(signal, 9 | 15) {
        return Err(AgentError::invalid("signal must be 9 or 15 on Windows"));
    }
    #[cfg(not(windows))]
    if !(1..=64).contains(&signal) {
        return Err(AgentError::invalid("signal must be in range 1..=64"));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn push_pkill_match(matched_pids: &mut Vec<i32>, pid: i32) -> AgentResult<()> {
    if matched_pids.len() == PKILL_MAX_TARGETS {
        return Err(AgentError::command(format!(
            "pkill matched more than {PKILL_MAX_TARGETS} processes"
        )));
    }
    matched_pids.push(pid);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn signal_pkill_matches(name: &str, signal: i32, mut matched_pids: Vec<i32>) -> AgentResult<Value> {
    matched_pids.sort_unstable();
    matched_pids.dedup();

    let mut signaled_pids = Vec::with_capacity(matched_pids.len());
    let mut failed_pids = Vec::new();
    for pid in matched_pids {
        if kill(pid, signal).is_ok() {
            signaled_pids.push(pid);
        } else {
            failed_pids.push(pid);
        }
    }
    let matched = signaled_pids.len() + failed_pids.len();
    Ok(json!({
        "name": name,
        "signal": signal,
        "matched": matched,
        "signaled_pids": signaled_pids,
        "failed_pids": failed_pids,
    }))
}

#[cfg(target_os = "linux")]
pub fn pkill(name: &str, signal: i32) -> AgentResult<Value> {
    use std::fs;

    validate_pkill_args(name, signal)?;
    let own_pid = std::process::id() as i32;
    let mut matched_pids = Vec::new();
    for entry in fs::read_dir("/proc").map_err(|error| AgentError::io("list /proc", error))? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|pid| *pid > 0 && *pid != own_pid)
        else {
            continue;
        };
        let Ok(mut process_name) = fs::read(entry.path().join("comm")) else {
            continue;
        };
        if process_name.last() == Some(&b'\n') {
            process_name.pop();
        }
        if process_name != name.as_bytes() {
            continue;
        }
        push_pkill_match(&mut matched_pids, pid)?;
    }
    signal_pkill_matches(name, signal, matched_pids)
}

#[cfg(windows)]
pub fn pkill(name: &str, signal: i32) -> AgentResult<Value> {
    validate_pkill_args(name, signal)?;
    let own_pid = std::process::id();
    let mut matched_pids = Vec::new();
    for entry in windows_process_entries()? {
        if entry.pid == own_pid || entry.name != name {
            continue;
        }
        let pid = i32::try_from(entry.pid)
            .map_err(|_| AgentError::command(format!("process PID {} is too large", entry.pid)))?;
        push_pkill_match(&mut matched_pids, pid)?;
    }
    signal_pkill_matches(name, signal, matched_pids)
}

#[cfg(target_os = "macos")]
pub fn pkill(name: &str, signal: i32) -> AgentResult<Value> {
    validate_pkill_args(name, signal)?;
    let own_pid = std::process::id();
    let mut matched_pids = Vec::new();
    for pid in macos_process_ids()?
        .into_iter()
        .filter(|pid| *pid != own_pid)
    {
        let pid = i32::try_from(pid)
            .map_err(|_| AgentError::command(format!("process PID {pid} is too large")))?;
        let Ok(info) = macos_bsd_info(pid) else {
            continue;
        };
        if macos_process_name(&info) == name {
            push_pkill_match(&mut matched_pids, pid)?;
        }
    }
    signal_pkill_matches(name, signal, matched_pids)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn pkill(name: &str, signal: i32) -> AgentResult<Value> {
    validate_pkill_args(name, signal)?;
    Err(AgentError::unsupported(
        "pkill requires Linux, macOS, or Windows",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod linux_pkill_tests {
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::pkill;

    #[test]
    fn linux_pkill_validates_arguments_and_reports_no_matches() {
        assert_eq!(pkill("", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(
            pkill("1234567890123456", 15).unwrap_err().kind,
            "invalid_params"
        );
        assert_eq!(pkill("has\0nul", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("valid", 0).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("valid", 65).unwrap_err().kind, "invalid_params");

        let result = pkill("rops-no-match", 15).unwrap();
        assert_eq!(result["matched"], 0);
        assert_eq!(result["signaled_pids"], serde_json::json!([]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
    }

    #[test]
    fn linux_pkill_exactly_matches_comm_and_defaults_to_sigterm() {
        let name = format!("rops-pk-{}", std::process::id());
        let directory = tempfile::tempdir().unwrap();
        let child_executable = directory.path().join(&name);
        std::fs::copy(std::env::current_exe().unwrap(), &child_executable).unwrap();
        let mut child = Command::new(child_executable)
            .args([
                "--exact",
                "tools::process::linux_pkill_tests::linux_pkill_test_child",
            ])
            .env("REMOTE_OPS_PKILL_TEST_NAME", &name)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let comm_path = format!("/proc/{pid}/comm");
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&comm_path)
            .ok()
            .is_none_or(|comm| comm.trim_end() != name)
        {
            assert!(
                Instant::now() < deadline,
                "test child did not expose the expected comm name"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let result = crate::dispatch("pkill", serde_json::json!({"name": name})).unwrap();
        assert_eq!(result["signal"], 15);
        assert_eq!(result["matched"], 1);
        assert_eq!(result["signaled_pids"], serde_json::json!([pid]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn linux_pkill_test_child() {
        if std::env::var_os("REMOTE_OPS_PKILL_TEST_NAME").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos", windows))))]
mod unsupported_pkill_tests {
    use super::pkill;

    #[test]
    fn pkill_is_unsupported_on_other_platforms_after_argument_validation() {
        assert_eq!(pkill("", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("remote-ops", 15).unwrap_err().kind, "unsupported");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{kill, pids, pkill, process_info};

    #[test]
    fn windows_pids_lists_and_filters_current_process() {
        let pid = std::process::id();
        let result = pids(None, None, 1024).unwrap();
        let current = result["processes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|process| process["pid"] == pid)
            .expect("current process should be listed");
        let name = current["name"].as_str().unwrap();
        assert!(!name.is_empty());
        assert!(!current["cmdline"].as_str().unwrap().is_empty());

        let filtered = pids(Some(name), None, 1024).unwrap();
        assert!(
            filtered["processes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|process| process["pid"] == pid)
        );
    }

    #[test]
    fn windows_pids_validates_and_paginates() {
        assert_eq!(pids(None, None, 0).unwrap_err().kind, "invalid_params");
        assert_eq!(
            pids(None, Some("not-a-pid"), 1).unwrap_err().kind,
            "invalid_params"
        );

        let first = pids(None, None, 1).unwrap();
        if first["truncated"].as_bool() == Some(true) {
            let cursor = first["next_cursor"].as_str().unwrap();
            let first_pid = first["processes"][0]["pid"].as_u64().unwrap();
            let second = pids(None, Some(cursor), 1).unwrap();
            let second_pid = second["processes"][0]["pid"].as_u64().unwrap();
            assert!(second_pid > first_pid);
        }
    }

    #[test]
    fn windows_process_info_reports_current_process() {
        let pid = std::process::id();
        let result = process_info(pid as i32).unwrap();
        assert_eq!(result["pid"], pid);
        assert!(!result["name"].as_str().unwrap().is_empty());
        assert!(result["state"].is_null());
        assert!(result["uid"].is_null());
        assert!(result["resident_bytes"].as_u64().unwrap() > 0);
        assert!(result["virtual_bytes"].as_u64().unwrap() > 0);
        assert!(result["start_time_seconds"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn windows_process_info_rejects_invalid_pid() {
        assert_eq!(process_info(0).unwrap_err().kind, "invalid_params");
        assert_eq!(process_info(i32::MAX).unwrap_err().kind, "io");
    }

    #[test]
    fn windows_kill_rejects_invalid_arguments() {
        assert_eq!(kill(0, 15).unwrap_err().kind, "invalid_params");
        assert_eq!(kill(1, 2).unwrap_err().kind, "invalid_params");
        assert_eq!(kill(i32::MAX, 15).unwrap_err().kind, "io");
    }

    #[test]
    fn windows_kill_terminates_process_for_supported_signals() {
        for (requested_signal, expected_signal) in [(Some(9), 9), (None, 15)] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::process::tests::windows_kill_test_child"])
                .env("REMOTE_OPS_KILL_TEST_CHILD", "1")
                .spawn()
                .unwrap();
            let pid = child.id() as i32;

            let result = match requested_signal {
                Some(signal) => kill(pid, signal).unwrap(),
                None => crate::dispatch("kill", serde_json::json!({"pid": pid})).unwrap(),
            };
            assert_eq!(result["pid"], pid);
            assert_eq!(result["signal"], expected_signal);
            assert!(!child.wait().unwrap().success());
        }
    }

    #[test]
    fn windows_pkill_validates_arguments_and_reports_no_matches() {
        assert_eq!(pkill("", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("has\0nul", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(
            pkill(&"x".repeat(261), 15).unwrap_err().kind,
            "invalid_params"
        );
        assert_eq!(pkill("valid.exe", 2).unwrap_err().kind, "invalid_params");

        let result = pkill("rops-no-match.exe", 15).unwrap();
        assert_eq!(result["matched"], 0);
        assert_eq!(result["signaled_pids"], serde_json::json!([]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
    }

    #[test]
    fn windows_pkill_exactly_matches_executable_name_and_defaults_to_termination() {
        let name = format!("rops-pk-{}.exe", std::process::id());
        let directory = tempfile::tempdir().unwrap();
        let child_executable = directory.path().join(&name);
        std::fs::copy(std::env::current_exe().unwrap(), &child_executable).unwrap();
        let mut child = Command::new(child_executable)
            .args(["--exact", "tools::process::tests::windows_pkill_test_child"])
            .env("REMOTE_OPS_PKILL_TEST_CHILD", "1")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_info(pid)
            .ok()
            .is_none_or(|info| info["name"] != name)
        {
            assert!(
                Instant::now() < deadline,
                "test child did not expose the expected executable name"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let result = crate::dispatch("pkill", serde_json::json!({"name": name})).unwrap();
        assert_eq!(result["signal"], 15);
        assert_eq!(result["matched"], 1);
        assert_eq!(result["signaled_pids"], serde_json::json!([pid]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn windows_kill_test_child() {
        if std::env::var_os("REMOTE_OPS_KILL_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn windows_pkill_test_child() {
        if std::env::var_os("REMOTE_OPS_PKILL_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{pids, pkill, process_info};

    #[test]
    fn macos_pids_lists_and_filters_current_process() {
        let pid = std::process::id();
        let result = pids(None, None, 1024).unwrap();
        let current = result["processes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|process| process["pid"] == pid)
            .expect("current process should be listed");
        let name = current["name"].as_str().unwrap();
        assert!(!name.is_empty());
        assert!(!current["cmdline"].as_str().unwrap().is_empty());

        let filtered = pids(Some(name), None, 1024).unwrap();
        assert!(
            filtered["processes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|process| process["pid"] == pid)
        );
    }

    #[test]
    fn macos_pids_validates_and_paginates() {
        assert_eq!(pids(None, None, 0).unwrap_err().kind, "invalid_params");
        assert_eq!(
            pids(None, Some("not-a-pid"), 1).unwrap_err().kind,
            "invalid_params"
        );

        let first = pids(None, None, 1).unwrap();
        if first["truncated"].as_bool() == Some(true) {
            let cursor = first["next_cursor"].as_str().unwrap();
            let first_pid = first["processes"][0]["pid"].as_u64().unwrap();
            let second = pids(None, Some(cursor), 1).unwrap();
            let second_pid = second["processes"][0]["pid"].as_u64().unwrap();
            assert!(second_pid > first_pid);
        }
    }

    #[test]
    fn macos_process_info_reports_current_process() {
        let pid = std::process::id();
        let result = process_info(pid as i32).unwrap();
        assert_eq!(result["pid"], pid);
        assert!(!result["name"].as_str().unwrap().is_empty());
        assert!(result["state"].as_str().is_some());
        assert!(result["uid"].as_u64().is_some());
        assert!(result["resident_bytes"].as_u64().unwrap() > 0);
        assert!(result["virtual_bytes"].as_u64().unwrap() > 0);
        assert!(result["start_time_seconds"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn macos_process_info_rejects_invalid_pid() {
        assert_eq!(process_info(0).unwrap_err().kind, "invalid_params");
        assert_eq!(process_info(i32::MAX).unwrap_err().kind, "io");
    }

    #[test]
    fn macos_pkill_validates_arguments_and_reports_no_matches() {
        assert_eq!(pkill("", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("has\0nul", 15).unwrap_err().kind, "invalid_params");
        assert_eq!(
            pkill(&"x".repeat(32), 15).unwrap_err().kind,
            "invalid_params"
        );
        assert_eq!(pkill("valid", 0).unwrap_err().kind, "invalid_params");
        assert_eq!(pkill("valid", 65).unwrap_err().kind, "invalid_params");

        let result = pkill("rops-no-match", 15).unwrap();
        assert_eq!(result["matched"], 0);
        assert_eq!(result["signaled_pids"], serde_json::json!([]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
    }

    #[test]
    fn macos_pkill_exactly_matches_process_name_and_defaults_to_sigterm() {
        let name = format!("rops-pk-{}", std::process::id());
        let directory = tempfile::tempdir().unwrap();
        let child_executable = directory.path().join(&name);
        std::fs::copy(std::env::current_exe().unwrap(), &child_executable).unwrap();
        let mut child = Command::new(child_executable)
            .args([
                "--exact",
                "tools::process::macos_tests::macos_pkill_test_child",
            ])
            .env("REMOTE_OPS_PKILL_TEST_CHILD", "1")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_info(pid)
            .ok()
            .is_none_or(|info| info["name"] != name)
        {
            assert!(
                Instant::now() < deadline,
                "test child did not expose the expected process name"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let result = crate::dispatch("pkill", serde_json::json!({"name": name})).unwrap();
        assert_eq!(result["signal"], 15);
        assert_eq!(result["matched"], 1);
        assert_eq!(result["signaled_pids"], serde_json::json!([pid]));
        assert_eq!(result["failed_pids"], serde_json::json!([]));
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn macos_pkill_test_child() {
        if std::env::var_os("REMOTE_OPS_PKILL_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }
}
