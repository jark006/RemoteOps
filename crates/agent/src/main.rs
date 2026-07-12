use std::env;
use std::net::TcpListener;
use std::time::Duration;

use remote_ops_agent::service::handle_connection;
use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;

struct Config {
    listen: String,
    timeout: Duration,
    max_transfer_bytes: u64,
}

fn main() {
    let config = parse_args().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    let listener = TcpListener::bind(&config.listen).unwrap_or_else(|error| {
        eprintln!("failed to listen on {}: {error}", config.listen);
        std::process::exit(1);
    });
    eprintln!("remote-ops-agent listening on {}", config.listen);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) =
                    handle_connection(stream, config.timeout, config.max_transfer_bytes)
                {
                    eprintln!("connection closed: {error}");
                }
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
}

fn parse_args() -> Result<Config, String> {
    let mut listen = "0.0.0.0:8022".to_string();
    let mut timeout_ms = 30_000u64;
    let mut max_transfer_bytes = DEFAULT_MAX_TRANSFER_BYTES;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().ok_or_else(|| "--listen requires a value".to_string())?,
            "--timeout-ms" => timeout_ms = parse_u64(&args.next().ok_or_else(|| "--timeout-ms requires a value".to_string())?, "--timeout-ms")?,
            "--max-transfer-bytes" => max_transfer_bytes = parse_u64(&args.next().ok_or_else(|| "--max-transfer-bytes requires a value".to_string())?, "--max-transfer-bytes")?,
            "--help" | "-h" => return Err("usage: remote-ops-agent [--listen 0.0.0.0:8022] [--timeout-ms 30000] [--max-transfer-bytes 4294967296]".to_string()),
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if timeout_ms == 0 || max_transfer_bytes == 0 {
        return Err("timeouts and transfer limit must be greater than zero".to_string());
    }
    Ok(Config {
        listen,
        timeout: Duration::from_millis(timeout_ms),
        max_transfer_bytes,
    })
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} requires a positive integer"))
}
