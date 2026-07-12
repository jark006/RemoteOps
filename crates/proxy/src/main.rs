use std::env;
use std::io::{self, BufRead, Write};
use std::net::SocketAddrV4;
use std::time::Duration;

use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;
use remote_ops_proxy::client::RemoteClient;
use remote_ops_proxy::mcp::handle_message;
use serde_json::{Value, json};

struct Config {
    remote: SocketAddrV4,
    timeout: Duration,
    max_transfer_bytes: u64,
}

fn main() {
    let config = parse_args().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    let mut client = RemoteClient::new(config.remote, config.timeout, config.max_transfer_bytes);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("stdin read failed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(message, &mut client),
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
            ),
        };
        if let Some(response) = response
            && (serde_json::to_writer(&mut stdout, &response).is_err()
                || stdout.write_all(b"\n").is_err()
                || stdout.flush().is_err())
        {
            break;
        }
    }
}

fn parse_args() -> Result<Config, String> {
    parse_args_from(env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut remote = "192.168.43.107:8022".parse().expect("valid default remote");
    let mut timeout_ms = 310_000u64;
    let mut max_transfer_bytes = DEFAULT_MAX_TRANSFER_BYTES;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--remote" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--remote requires IPv4:PORT".to_string())?;
                remote = value
                    .parse::<SocketAddrV4>()
                    .map_err(|_| "--remote requires a valid IPv4:PORT".to_string())?;
                if remote.port() == 0 {
                    return Err("--remote port must be in range 1..=65535".to_string());
                }
            }
            "--timeout-ms" => timeout_ms = parse_u64(args.next(), "--timeout-ms")?,
            "--max-transfer-bytes" => max_transfer_bytes = parse_u64(args.next(), "--max-transfer-bytes")?,
            "--help" | "-h" => return Err("usage: remote-ops-proxy [--remote IPv4:PORT] [--timeout-ms 310000] [--max-transfer-bytes 4294967296]".to_string()),
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if timeout_ms == 0 || max_transfer_bytes == 0 {
        return Err("timeouts and transfer limit must be greater than zero".to_string());
    }
    Ok(Config {
        remote,
        timeout: Duration::from_millis(timeout_ms),
        max_transfer_bytes,
    })
}

fn parse_u64(value: Option<String>, name: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("{name} requires a value"))?
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
    fn remote_defaults_and_can_be_overridden() {
        assert_eq!(args(&[]).unwrap().remote.to_string(), "192.168.43.107:8022");
        assert_eq!(
            args(&["--remote", "127.0.0.1:9000"])
                .unwrap()
                .remote
                .to_string(),
            "127.0.0.1:9000"
        );
    }

    #[test]
    fn remote_requires_valid_ipv4_and_nonzero_port() {
        for values in [
            &["--remote"][..],
            &["--remote", "localhost:8022"][..],
            &["--remote", "127.0.0.1"][..],
            &["--remote", "127.0.0.1:0"][..],
            &["--remote", "127.0.0.1:65536"][..],
        ] {
            assert!(args(values).is_err(), "accepted {values:?}");
        }
    }
}
