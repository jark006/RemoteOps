use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use serde_json::{Value, json};

use crate::error::{AgentError, AgentResult};

use super::{
    MAX_LISTENING_PORTS, MAX_MOUNTS, MAX_ROUTES, MAX_SYSTEM_FILE_BYTES, bounded_text, collection,
    cpu_snapshot, interface_snapshot, logical_core_count, resolv_conf_snapshot, toolchain_snapshot,
    unix_identity, unix_time_snapshot,
};

const CAPABILITY_NAMES: &[&str] = &[
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_DAC_READ_SEARCH",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_LINUX_IMMUTABLE",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_BROADCAST",
    "CAP_NET_ADMIN",
    "CAP_NET_RAW",
    "CAP_IPC_LOCK",
    "CAP_IPC_OWNER",
    "CAP_SYS_MODULE",
    "CAP_SYS_RAWIO",
    "CAP_SYS_CHROOT",
    "CAP_SYS_PTRACE",
    "CAP_SYS_PACCT",
    "CAP_SYS_ADMIN",
    "CAP_SYS_BOOT",
    "CAP_SYS_NICE",
    "CAP_SYS_RESOURCE",
    "CAP_SYS_TIME",
    "CAP_SYS_TTY_CONFIG",
    "CAP_MKNOD",
    "CAP_LEASE",
    "CAP_AUDIT_WRITE",
    "CAP_AUDIT_CONTROL",
    "CAP_SETFCAP",
    "CAP_MAC_OVERRIDE",
    "CAP_MAC_ADMIN",
    "CAP_SYSLOG",
    "CAP_WAKE_ALARM",
    "CAP_BLOCK_SUSPEND",
    "CAP_AUDIT_READ",
    "CAP_PERFMON",
    "CAP_BPF",
    "CAP_CHECKPOINT_RESTORE",
];

pub fn system_info() -> AgentResult<Value> {
    let hostname = hostname()?;
    let kernel = kernel()?;
    let uptime = bounded_text("/proc/uptime", 4096)
        .and_then(|(value, _)| value.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    let loads: Vec<f64> = bounded_text("/proc/loadavg", 4096)
        .map(|(value, _)| {
            value
                .split_whitespace()
                .take(3)
                .filter_map(|field| field.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let meminfo = bounded_text("/proc/meminfo", 256 * 1024)
        .map(|(value, _)| value)
        .unwrap_or_default();
    let memory_value = |key: &str| {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|value| value.saturating_mul(1024))
    };
    let root =
        statvfs("/").ok_or_else(|| AgentError::io("statvfs /", std::io::Error::last_os_error()))?;
    let (mounts, mounts_available, mounts_truncated) = mount_snapshot();
    let cpuinfo = bounded_text("/proc/cpuinfo", MAX_SYSTEM_FILE_BYTES)
        .map(|(value, _)| value)
        .unwrap_or_default();
    let status = bounded_text("/proc/self/status", 256 * 1024)
        .map(|(value, _)| value)
        .unwrap_or_default();
    let routes = route_snapshot();
    let listening_ports = listening_port_snapshot();
    let umask = status_value(&status, "Umask").map(str::to_string);

    Ok(json!({
        "hostname": hostname,
        "kernel": kernel,
        "uptime_seconds": uptime,
        "load_average": {
            "one": loads.first().copied().unwrap_or(0.0),
            "five": loads.get(1).copied().unwrap_or(0.0),
            "fifteen": loads.get(2).copied().unwrap_or(0.0)
        },
        "memory": {
            "total_bytes": memory_value("MemTotal").unwrap_or(0),
            "available_bytes": memory_value("MemAvailable").unwrap_or(0)
        },
        "root_filesystem": {
            "total_bytes": root.total_bytes,
            "available_bytes": root.available_bytes
        },
        "temperatures": temperatures(),
        "os": os_release_snapshot(),
        "cpu": cpu_snapshot(
            cpu_model(&cpuinfo),
            online_logical_cores(),
            physical_core_count(),
            linux_libc_family(),
            linux_libc_version()
        ),
        "identity": unix_identity(capability_snapshot(&status), umask),
        "network": {
            "interfaces": interface_snapshot(),
            "routes": routes,
            "dns": resolv_conf_snapshot(),
            "listening_ports": listening_ports
        },
        "filesystems": {
            "available": mounts_available,
            "mounts": mounts,
            "truncated": mounts_truncated
        },
        "time": unix_time_snapshot(),
        "init_system": init_system_snapshot(),
        "toolchains": toolchain_snapshot()
    }))
}

fn hostname() -> AgentResult<String> {
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

fn kernel() -> AgentResult<Value> {
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
        "machine": cstr(&uts.machine)
    }))
}

fn os_release_snapshot() -> Value {
    let contents = bounded_text("/etc/os-release", 64 * 1024)
        .or_else(|| bounded_text("/usr/lib/os-release", 64 * 1024))
        .map(|(value, _)| value)
        .unwrap_or_default();
    let fields = parse_os_release(&contents);
    let value = |key: &str| fields.get(key).cloned();
    let id_like = value("ID_LIKE")
        .map(|value| {
            value
                .split_whitespace()
                .take(16)
                .map(|item| item.chars().take(128).collect::<String>())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "id": value("ID"),
        "id_like": id_like,
        "name": value("NAME"),
        "pretty_name": value("PRETTY_NAME"),
        "version": value("VERSION"),
        "version_id": value("VERSION_ID"),
        "version_codename": value("VERSION_CODENAME"),
        "variant": value("VARIANT"),
        "variant_id": value("VARIANT_ID"),
        "build_id": value("BUILD_ID"),
        "image_id": value("IMAGE_ID"),
        "image_version": value("IMAGE_VERSION")
    })
}

fn parse_os_release(contents: &str) -> BTreeMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            if key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
            {
                return None;
            }
            Some((key.to_string(), unquote_os_release(value)))
        })
        .take(64)
        .collect()
}

fn unquote_os_release(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].chars().take(4096).collect();
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut output = String::new();
        let mut characters = value[1..value.len() - 1].chars();
        while let Some(character) = characters.next() {
            if character == '\\' {
                match characters.next() {
                    Some(escaped @ ('"' | '\\' | '$' | '`')) => output.push(escaped),
                    Some(other) => {
                        output.push('\\');
                        output.push(other);
                    }
                    None => output.push('\\'),
                }
            } else {
                output.push(character);
            }
            if output.len() >= 4096 {
                break;
            }
        }
        return output;
    }
    value.chars().take(4096).collect()
}

fn cpu_model(cpuinfo: &str) -> Option<String> {
    ["model name", "Model", "Hardware", "Processor", "cpu model"]
        .into_iter()
        .find_map(|key| {
            cpuinfo.lines().find_map(|line| {
                let (candidate, value) = line.split_once(':')?;
                (candidate.trim() == key)
                    .then(|| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
        })
}

fn online_logical_cores() -> u64 {
    let count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if count > 0 {
        u64::try_from(count).unwrap_or(u64::MAX)
    } else {
        logical_core_count()
    }
}

fn physical_core_count() -> Option<u64> {
    let entries = fs::read_dir("/sys/devices/system/cpu").ok()?;
    let mut cores = BTreeSet::new();
    for entry in entries.flatten().take(1024) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.strip_prefix("cpu").is_none_or(|suffix| {
            suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            continue;
        }
        if bounded_text(entry.path().join("online"), 16)
            .is_some_and(|(value, _)| value.trim() == "0")
        {
            continue;
        }
        let topology = entry.path().join("topology");
        let Some(package) = bounded_text(topology.join("physical_package_id"), 64)
            .map(|(value, _)| value.trim().to_string())
        else {
            continue;
        };
        let Some(core) =
            bounded_text(topology.join("core_id"), 64).map(|(value, _)| value.trim().to_string())
        else {
            continue;
        };
        cores.insert((package, core));
    }
    (!cores.is_empty()).then(|| u64::try_from(cores.len()).unwrap_or(u64::MAX))
}

fn linux_libc_family() -> &'static str {
    if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "gnu") {
        "glibc"
    } else {
        "unknown"
    }
}

#[cfg(target_env = "gnu")]
fn linux_libc_version() -> Option<String> {
    let required = unsafe { libc::confstr(libc::_CS_GNU_LIBC_VERSION, std::ptr::null_mut(), 0) };
    if required == 0 || required > 4096 {
        return None;
    }
    let mut buffer = vec![0 as libc::c_char; required];
    if unsafe {
        libc::confstr(
            libc::_CS_GNU_LIBC_VERSION,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    } == 0
    {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
    Some(value.strip_prefix("glibc ").unwrap_or(&value).to_string())
}

#[cfg(not(target_env = "gnu"))]
fn linux_libc_version() -> Option<String> {
    None
}

fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

fn capability_snapshot(status: &str) -> Value {
    let last_capability = bounded_text("/proc/sys/kernel/cap_last_cap", 64)
        .and_then(|(value, _)| value.trim().parse::<u32>().ok())
        .unwrap_or(40)
        .min(63);
    json!({
        "inheritable": capability_set(status_value(status, "CapInh"), last_capability),
        "permitted": capability_set(status_value(status, "CapPrm"), last_capability),
        "effective": capability_set(status_value(status, "CapEff"), last_capability),
        "bounding": capability_set(status_value(status, "CapBnd"), last_capability),
        "ambient": capability_set(status_value(status, "CapAmb"), last_capability),
        "last_capability": last_capability
    })
}

fn capability_set(mask: Option<&str>, last_capability: u32) -> Value {
    let mask = mask.unwrap_or("0");
    let value = u64::from_str_radix(mask, 16).unwrap_or(0);
    let names = (0..=last_capability)
        .filter(|bit| value & (1u64 << bit) != 0)
        .map(|bit| {
            CAPABILITY_NAMES
                .get(bit as usize)
                .map(|name| (*name).to_string())
                .unwrap_or_else(|| format!("CAP_{bit}"))
        })
        .collect::<Vec<_>>();
    json!({"mask": mask, "names": names})
}

fn route_snapshot() -> Value {
    let mut routes = Vec::new();
    let mut available = false;
    let mut truncated = false;
    if let Some((contents, file_truncated)) = bounded_text("/proc/net/route", MAX_SYSTEM_FILE_BYTES)
    {
        available = true;
        truncated |= file_truncated;
        for line in contents.lines().skip(1) {
            if routes.len() >= MAX_ROUTES {
                truncated = true;
                break;
            }
            if let Some(route) = parse_ipv4_route(line) {
                routes.push(route);
            }
        }
    }
    if routes.len() < MAX_ROUTES
        && let Some((contents, file_truncated)) =
            bounded_text("/proc/net/ipv6_route", MAX_SYSTEM_FILE_BYTES)
    {
        available = true;
        truncated |= file_truncated;
        for line in contents.lines() {
            if routes.len() >= MAX_ROUTES {
                truncated = true;
                break;
            }
            if let Some(route) = parse_ipv6_route(line) {
                routes.push(route);
            }
        }
    }
    collection(available, routes, truncated)
}

fn parse_ipv4_route(line: &str) -> Option<Value> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 11 {
        return None;
    }
    let destination = parse_proc_ipv4(fields[1])?;
    let gateway = parse_proc_ipv4(fields[2])?;
    let flags = u32::from_str_radix(fields[3], 16).ok()?;
    let metric = fields[6].parse::<u64>().ok()?;
    let mask = parse_proc_ipv4(fields[7])?;
    let prefix_length = u32::from(mask).count_ones();
    Some(json!({
        "family": "ipv4",
        "destination": format!("{destination}/{prefix_length}"),
        "gateway": (gateway != Ipv4Addr::UNSPECIFIED).then(|| gateway.to_string()),
        "interface": fields[0].chars().take(256).collect::<String>(),
        "metric": metric,
        "flags": flags
    }))
}

fn parse_proc_ipv4(value: &str) -> Option<Ipv4Addr> {
    let value = u32::from_str_radix(value, 16).ok()?;
    Some(Ipv4Addr::from(value.to_le_bytes()))
}

fn parse_ipv6_route(line: &str) -> Option<Value> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 10 {
        return None;
    }
    let destination = parse_network_ipv6(fields[0])?;
    let prefix_length = u8::from_str_radix(fields[1], 16).ok()?;
    let gateway = parse_network_ipv6(fields[4])?;
    let metric = u64::from_str_radix(fields[5], 16).ok()?;
    let flags = u32::from_str_radix(fields[8], 16).ok()?;
    Some(json!({
        "family": "ipv6",
        "destination": format!("{destination}/{prefix_length}"),
        "gateway": (gateway != Ipv6Addr::UNSPECIFIED).then(|| gateway.to_string()),
        "interface": fields[9].chars().take(256).collect::<String>(),
        "metric": metric,
        "flags": flags
    }))
}

fn parse_network_ipv6(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(Ipv6Addr::from(bytes))
}

fn listening_port_snapshot() -> Value {
    let sources = [
        ("/proc/net/tcp", "tcp", false),
        ("/proc/net/tcp6", "tcp", true),
        ("/proc/net/udp", "udp", false),
        ("/proc/net/udp6", "udp", true),
    ];
    let mut available = false;
    let mut truncated = false;
    let mut ports = BTreeMap::<String, Value>::new();
    for (path, protocol, ipv6) in sources {
        let Some((contents, file_truncated)) = bounded_text(path, MAX_SYSTEM_FILE_BYTES) else {
            continue;
        };
        available = true;
        truncated |= file_truncated;
        for line in contents.lines().skip(1) {
            if ports.len() >= MAX_LISTENING_PORTS {
                truncated = true;
                break;
            }
            if let Some((key, port)) = parse_socket(line, protocol, ipv6) {
                ports.entry(key).or_insert(port);
            }
        }
    }
    collection(available, ports.into_values().collect(), truncated)
}

fn parse_socket(line: &str, protocol: &str, ipv6: bool) -> Option<(String, Value)> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 4 || (protocol == "tcp" && fields[3] != "0A") {
        return None;
    }
    let (address, port) = parse_proc_endpoint(fields[1], ipv6)?;
    let (remote_address, remote_port) = parse_proc_endpoint(fields[2], ipv6)?;
    if protocol == "udp"
        && (remote_port != 0 || remote_address != if ipv6 { "::" } else { "0.0.0.0" })
    {
        return None;
    }
    if port == 0 {
        return None;
    }
    let family = if ipv6 { "ipv6" } else { "ipv4" };
    let key = format!("{protocol}:{family}:{address}:{port}");
    Some((
        key,
        json!({
            "protocol": protocol,
            "family": family,
            "local_address": address,
            "port": port
        }),
    ))
}

fn parse_proc_endpoint(value: &str, ipv6: bool) -> Option<(String, u16)> {
    let (address, port) = value.rsplit_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    if ipv6 {
        if address.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for word in 0..4 {
            let raw = u32::from_str_radix(&address[word * 8..word * 8 + 8], 16).ok()?;
            bytes[word * 4..word * 4 + 4].copy_from_slice(&raw.to_le_bytes());
        }
        Some((Ipv6Addr::from(bytes).to_string(), port))
    } else {
        Some((parse_proc_ipv4(address)?.to_string(), port))
    }
}

#[derive(Clone, Copy)]
struct FilesystemStats {
    total_bytes: u64,
    available_bytes: u64,
    total_inodes: u64,
    available_inodes: u64,
}

fn mount_snapshot() -> (Vec<Value>, bool, bool) {
    let Some((contents, file_truncated)) =
        bounded_text("/proc/self/mountinfo", MAX_SYSTEM_FILE_BYTES)
    else {
        return (Vec::new(), false, false);
    };
    let mut mounts = Vec::new();
    let mut truncated = file_truncated;
    for line in contents.lines() {
        if mounts.len() >= MAX_MOUNTS {
            truncated = true;
            break;
        }
        if let Some(mount) = parse_mount(line) {
            mounts.push(mount);
        }
    }
    mounts.sort_by(|left, right| {
        left["mount_point"]
            .as_str()
            .cmp(&right["mount_point"].as_str())
    });
    (mounts, true, truncated)
}

fn parse_mount(line: &str) -> Option<Value> {
    let (mount_fields, filesystem_fields) = line.split_once(" - ")?;
    let mount_fields: Vec<_> = mount_fields.split_whitespace().collect();
    let filesystem_fields: Vec<_> = filesystem_fields.split_whitespace().collect();
    if mount_fields.len() < 6 || filesystem_fields.len() < 2 {
        return None;
    }
    let mount_point = decode_mount_field(mount_fields[4]);
    let source = decode_mount_field(filesystem_fields[1]);
    let fs_type = filesystem_fields[0].chars().take(128).collect::<String>();
    let read_only = mount_fields[5].split(',').any(|option| option == "ro");
    let stats = statvfs(&mount_point);
    Some(json!({
        "source": source,
        "mount_point": mount_point,
        "fs_type": fs_type,
        "total_bytes": stats.map(|value| value.total_bytes),
        "available_bytes": stats.map(|value| value.available_bytes),
        "total_inodes": stats.map(|value| value.total_inodes),
        "available_inodes": stats.map(|value| value.available_inodes),
        "read_only": read_only
    }))
}

fn decode_mount_field(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() && output.len() < 1024 {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &bytes[index + 1..index + 4];
            if octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                output.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + octal[2] - b'0');
                index += 4;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn statvfs(path: &str) -> Option<FilesystemStats> {
    let path = CString::new(path.as_bytes()).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat_value_to_u64(stat.f_frsize);
    Some(FilesystemStats {
        total_bytes: stat_value_to_u64(stat.f_blocks).saturating_mul(block_size),
        available_bytes: stat_value_to_u64(stat.f_bavail).saturating_mul(block_size),
        total_inodes: stat_value_to_u64(stat.f_files),
        available_inodes: stat_value_to_u64(stat.f_favail),
    })
}

fn stat_value_to_u64(value: impl TryInto<u64>) -> u64 {
    value.try_into().ok().unwrap_or(u64::MAX)
}

fn temperatures() -> Vec<Value> {
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
            let raw = bounded_text(path.join("temp"), 64)?
                .0
                .trim()
                .parse::<f64>()
                .ok()?;
            let name = bounded_text(path.join("type"), 256)?.0.trim().to_string();
            Some(json!({"name": name, "celsius": raw / 1000.0}))
        })
        .collect()
}

fn init_system_snapshot() -> Value {
    let pid1_comm = bounded_text("/proc/1/comm", 256)
        .map(|(value, _)| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let init_target = fs::read_link("/sbin/init")
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let name =
        if Path::new("/run/systemd/system").exists() || pid1_comm.as_deref() == Some("systemd") {
            Some("systemd")
        } else if Path::new("/run/openrc").exists()
            || init_target
                .as_deref()
                .is_some_and(|path| path.contains("openrc"))
        {
            Some("openrc")
        } else if pid1_comm.as_deref() == Some("busybox")
            || init_target
                .as_deref()
                .is_some_and(|path| path.contains("busybox"))
        {
            Some("busybox-init")
        } else if pid1_comm.as_deref() == Some("init") {
            Some("sysvinit")
        } else {
            pid1_comm.as_deref()
        };
    json!({"name": name, "pid1_comm": pid1_comm})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_os_release_fields() {
        let fields = parse_os_release(
            "ID=debian\nID_LIKE=\"debian rhel\"\nPRETTY_NAME=\"Demo \\\"Board\\\"\"\n",
        );
        assert_eq!(fields["ID"], "debian");
        assert_eq!(fields["ID_LIKE"], "debian rhel");
        assert_eq!(fields["PRETTY_NAME"], "Demo \"Board\"");
    }

    #[test]
    fn parses_ipv4_and_ipv6_routes() {
        let ipv4 = parse_ipv4_route("eth0 00000000 0101A8C0 0003 0 0 100 00000000 0 0 0").unwrap();
        assert_eq!(ipv4["destination"], "0.0.0.0/0");
        assert_eq!(ipv4["gateway"], "192.168.1.1");
        let ipv6 = parse_ipv6_route(
            "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000000 00000000 00000003 eth0",
        )
        .unwrap();
        assert_eq!(ipv6["destination"], "::/0");
        assert_eq!(ipv6["gateway"], "fe80::1");
    }

    #[test]
    fn parses_listening_socket_addresses() {
        let (_, tcp) = parse_socket(
            "0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 0 1",
            "tcp",
            false,
        )
        .unwrap();
        assert_eq!(tcp["local_address"], "127.0.0.1");
        assert_eq!(tcp["port"], 8080);
        let (_, tcp6) = parse_socket(
            "0: 00000000000000000000000000000000:01BB 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 0 1",
            "tcp",
            true,
        )
        .unwrap();
        assert_eq!(tcp6["local_address"], "::");
        assert_eq!(tcp6["port"], 443);
    }

    #[test]
    fn parses_mount_escapes_and_stats_shape() {
        assert_eq!(decode_mount_field("/media/My\\040Disk"), "/media/My Disk");
        let mount =
            parse_mount("31 23 8:1 / /definitely-missing\\040mount ro,nosuid - ext4 /dev/sda1 ro")
                .unwrap();
        assert_eq!(mount["mount_point"], "/definitely-missing mount");
        assert_eq!(mount["fs_type"], "ext4");
        assert_eq!(mount["read_only"], true);
        assert!(mount["total_bytes"].is_null());
    }

    #[test]
    fn decodes_capability_masks_to_names() {
        let set = capability_set(Some("0000000000000401"), 40);
        assert_eq!(set["names"], json!(["CAP_CHOWN", "CAP_NET_BIND_SERVICE"]));
    }

    #[test]
    fn linux_system_info_reports_extended_shape() {
        let result = system_info().unwrap();
        assert_eq!(result["kernel"]["sysname"], "Linux");
        assert!(result["os"].is_object());
        assert!(result["cpu"]["logical_cores"].as_u64().unwrap() > 0);
        assert!(result["identity"]["effective_user"]["id"].is_u64());
        assert!(result["identity"]["capabilities"]["effective"]["names"].is_array());
        assert!(result["network"]["interfaces"]["items"].is_array());
        assert!(result["network"]["routes"]["items"].is_array());
        assert!(result["network"]["listening_ports"]["items"].is_array());
        assert!(result["filesystems"]["mounts"].is_array());
        assert!(result["time"]["unix_seconds"].as_u64().unwrap() > 0);
        assert!(result["init_system"].is_object());
        assert!(result["toolchains"]["items"].is_array());
    }
}
