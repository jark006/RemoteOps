use std::env;
use std::net::TcpListener;

use remote_ops_agent::service::handle_connection;
use remote_ops_protocol::DEFAULT_MAX_TRANSFER_BYTES;

struct Config {
    listen: String,
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
                if let Err(error) = handle_connection(stream, config.max_transfer_bytes) {
                    eprintln!("connection closed: {error}");
                }
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
}

fn parse_args() -> Result<Config, String> {
    parse_args_from(env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut listen = "0.0.0.0:8022".to_string();
    let mut max_transfer_bytes = DEFAULT_MAX_TRANSFER_BYTES;
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
    }

    #[test]
    fn legacy_timeout_argument_is_rejected() {
        assert!(args(&["--timeout-ms", "1000"]).is_err());
    }
}
