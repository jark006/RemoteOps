use std::env;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;

use remote_ops_agent::service::{ConnectionAction, handle_connection};
use remote_ops_agent::tools::jobs::JobManager;
use remote_ops_agent::tools::lifecycle;
use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;

struct Config {
    listen: String,
    max_transfer_bytes: u64,
    update_health_file: Option<PathBuf>,
    cleanup_update_helper: Option<PathBuf>,
}

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--self-check" {
        println!("{}", lifecycle::self_check_info());
        return;
    }
    if arguments.first().map(String::as_str) == Some("--update-helper") {
        if arguments.len() != 2 {
            eprintln!("--update-helper requires exactly one manifest path");
            std::process::exit(2);
        }
        if let Err(error) = lifecycle::run_update_helper(PathBuf::from(&arguments[1]).as_path()) {
            eprintln!("agent update failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    let config = parse_args_from(arguments).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    lifecycle::configure_restart_args(vec![
        "--listen".to_string(),
        config.listen.clone(),
        "--max-transfer-bytes".to_string(),
        config.max_transfer_bytes.to_string(),
    ]);
    let listener = TcpListener::bind(&config.listen).unwrap_or_else(|error| {
        eprintln!("failed to listen on {}: {error}", config.listen);
        std::process::exit(1);
    });
    let local_addr = listener.local_addr().unwrap_or_else(|error| {
        eprintln!("failed to determine listening address: {error}");
        std::process::exit(1);
    });
    print_listening_addresses(local_addr);
    if let Some(path) = &config.update_health_file
        && let Err(error) = lifecycle::write_health_marker(path, config.max_transfer_bytes)
    {
        eprintln!("failed to write update health marker: {error}");
        std::process::exit(1);
    }
    if let Some(path) = config.cleanup_update_helper.clone() {
        lifecycle::schedule_cleanup(vec![path]);
    }
    let jobs = JobManager::new();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => match handle_connection(stream, config.max_transfer_bytes, &jobs) {
                Ok(ConnectionAction::Continue) => {}
                Ok(ConnectionAction::RestartAgent) => return,
                Err(error) => eprintln!("connection closed: {error}"),
            },
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
}

fn print_listening_addresses(bind_addr: SocketAddr) {
    let interface_ips = if bind_addr.ip().is_unspecified() {
        match if_addrs::get_if_addrs() {
            Ok(interfaces) => interfaces
                .into_iter()
                .map(|interface| interface.ip())
                .collect(),
            Err(error) => {
                eprintln!("failed to enumerate local IP addresses: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let addresses = listening_addresses(bind_addr, interface_ips);
    for address in addresses {
        eprintln!("remote-ops-agent listening on {address}");
    }
}

fn listening_addresses<I>(bind_addr: SocketAddr, interface_ips: I) -> Vec<SocketAddr>
where
    I: IntoIterator<Item = IpAddr>,
{
    if !bind_addr.ip().is_unspecified() {
        return vec![bind_addr];
    }

    let is_ipv4 = bind_addr.ip().is_ipv4();
    let mut addresses: Vec<_> = interface_ips
        .into_iter()
        .filter(|ip| ip.is_ipv4() == is_ipv4 && !ip.is_unspecified() && !ip.is_loopback())
        .map(|ip| SocketAddr::new(ip, bind_addr.port()))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.is_empty() {
        addresses.push(bind_addr);
    }
    addresses
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut listen = "0.0.0.0:8022".to_string();
    let mut max_transfer_bytes = DEFAULT_MAX_TRANSFER_BYTES;
    let mut update_health_file = None;
    let mut cleanup_update_helper = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                listen = args
                    .next()
                    .ok_or_else(|| "--listen requires a value".to_string())?
            }
            "--max-transfer-bytes" => {
                max_transfer_bytes = parse_u64(
                    &args
                        .next()
                        .ok_or_else(|| "--max-transfer-bytes requires a value".to_string())?,
                    "--max-transfer-bytes",
                )?
            }
            "--update-health-file" => {
                update_health_file =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--update-health-file requires a value".to_string()
                    })?));
            }
            "--cleanup-update-helper" => {
                cleanup_update_helper =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--cleanup-update-helper requires a value".to_string()
                    })?));
            }
            "--help" | "-h" => return Err(
                "usage: remote-ops-agent [--listen 0.0.0.0:8022] [--max-transfer-bytes 4294967296]"
                    .to_string(),
            ),
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if max_transfer_bytes == 0 {
        return Err("transfer limit must be greater than zero".to_string());
    }
    Ok(Config {
        listen,
        max_transfer_bytes,
        update_health_file,
        cleanup_update_helper,
    })
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} requires a positive integer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<Config, String> {
        parse_args_from(values.iter().map(|value| value.to_string()))
    }

    #[test]
    fn defaults_do_not_require_a_socket_timeout() {
        let config = args(&[]).unwrap();
        assert_eq!(config.listen, "0.0.0.0:8022");
        assert_eq!(config.max_transfer_bytes, DEFAULT_MAX_TRANSFER_BYTES);
        assert!(config.update_health_file.is_none());
        assert!(config.cleanup_update_helper.is_none());
    }

    #[test]
    fn legacy_timeout_argument_is_rejected() {
        assert!(args(&["--timeout-ms", "1000"]).is_err());
    }

    #[test]
    fn explicit_listen_address_is_preserved() {
        let address: SocketAddr = "127.0.0.1:8022".parse().unwrap();
        assert_eq!(
            listening_addresses(address, ["192.168.1.10".parse().unwrap()]),
            vec![address]
        );
    }

    #[test]
    fn wildcard_listen_address_expands_to_matching_interface_ips() {
        let address: SocketAddr = "0.0.0.0:8022".parse().unwrap();
        let interfaces = [
            "192.168.1.10".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
            "192.168.1.10".parse().unwrap(),
            "fe80::1".parse().unwrap(),
        ];
        let expected = vec!["192.168.1.10:8022".parse().unwrap()];
        assert_eq!(listening_addresses(address, interfaces), expected);
    }

    #[test]
    fn wildcard_listen_address_has_a_fallback_when_enumeration_is_empty() {
        let address: SocketAddr = "0.0.0.0:8022".parse().unwrap();
        assert_eq!(listening_addresses(address, []), vec![address]);
    }
}
