use remote_ops_protocol::encoding::{JsonEncodeError, to_vec_bounded};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

use crate::{
    client::{ClientError, RemoteClient},
    mcp::handle_message_with_response_mode,
    response::{self, ResponseMode},
};

pub const MAX_MCP_REQUEST_BYTES: usize = 40 * 1024 * 1024;
pub const MAX_MCP_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

enum Input {
    Eof,
    Message(Vec<u8>),
    TooLarge,
}

fn read_message(reader: &mut impl BufRead, limit: usize) -> io::Result<Input> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut seen = false;
    let mut exceeded = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if !seen {
                Input::Eof
            } else if exceeded {
                Input::TooLarge
            } else {
                Input::Message(bytes)
            });
        }
        seen = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(available.len());
        if !exceeded {
            if length > limit.saturating_sub(bytes.len()) {
                exceeded = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..length]);
            }
        }
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(if exceeded {
                Input::TooLarge
            } else {
                Input::Message(bytes)
            });
        }
    }
}

pub fn serve(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    client: &mut RemoteClient,
    mode: ResponseMode,
) -> io::Result<()> {
    serve_with_limits(
        reader,
        writer,
        client,
        mode,
        MAX_MCP_REQUEST_BYTES,
        MAX_MCP_RESPONSE_BYTES,
    )
}

fn serve_with_limits(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    client: &mut RemoteClient,
    mode: ResponseMode,
    input_limit: usize,
    output_limit: usize,
) -> io::Result<()> {
    loop {
        let before = client.operation_attempts();
        let mut tool_call = false;
        let response = match read_message(reader, input_limit)? {
            Input::Eof => return Ok(()),
            Input::TooLarge => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"MCP request exceeds byte limit","data":{"max_bytes":input_limit}}}),
            ),
            Input::Message(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => continue,
            Input::Message(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(message) => {
                    tool_call = message["method"] == "tools/call";
                    handle_message_with_response_mode(message, client, mode)
                }
                Err(_) => Some(
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
                ),
            },
        };
        let Some(response) = response else { continue };
        let encoded = match to_vec_bounded(&response, output_limit) {
            Ok(encoded) => encoded,
            Err(JsonEncodeError::TooLarge(_)) => {
                let error=ClientError::response_limited("tool or request finished but its MCP response exceeds the byte limit; reduce requested output; requests were not replayed").with_request_started(client.operation_attempts()>before);
                let fallback = if tool_call && response.get("result").is_some() {
                    json!({"jsonrpc":"2.0","id":response["id"],"result":response::failed_result(&error)})
                } else {
                    json!({"jsonrpc":"2.0","id":response["id"],"error":{"code":-32603,"message":"MCP response exceeds byte limit","data":error.details()}})
                };
                to_vec_bounded(&fallback, output_limit).map_err(io::Error::other)?
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        writer.write_all(&encoded)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(input: &[u8], input_limit: usize, output_limit: usize) -> Vec<Value> {
        let mut client = RemoteClient::new(
            "127.0.0.1:1".parse().unwrap(),
            std::time::Duration::from_millis(10),
            1024,
        );
        let mut output = Vec::new();
        serve_with_limits(
            &mut io::Cursor::new(input),
            &mut output,
            &mut client,
            ResponseMode::Full,
            input_limit,
            output_limit,
        )
        .unwrap();
        output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }
    #[test]
    fn oversized_and_invalid_messages_are_drained_before_the_next_request() {
        let mut input = vec![b'x'; 1025];
        input.extend_from_slice(b"\n\xff\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n");
        let output = run(&input, 1024, 2048);
        assert_eq!(output.len(), 3);
        assert_eq!(output[0]["error"]["code"], -32600);
        assert_eq!(output[1]["error"]["code"], -32700);
        assert_eq!(output[2]["id"], 3);
        assert_eq!(output[2]["result"], json!({}));
    }
    #[test]
    fn oversized_discovery_response_does_not_destroy_stdio() {
        let output=run(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",1024,512);
        assert_eq!(output[0]["error"]["data"]["outcome"], "response_limited");
        assert_eq!(output[0]["error"]["data"]["request_started"], false);
        assert_eq!(output[1]["result"], json!({}));
    }

    #[test]
    fn tool_output_limit_is_a_tool_error_and_allows_following_messages() {
        let calls = vec![json!({"tool":"remote_status"}); 16];
        let message = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"batch","arguments":{"calls":calls}}});
        let input = format!("{message}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}}\n");
        let output = run(input.as_bytes(), 4096, 2048);
        assert_eq!(output[0]["result"]["isError"], true);
        let error: Value =
            serde_json::from_str(output[0]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(error["error"]["outcome"], "response_limited");
        assert_eq!(error["error"]["request_started"], false);
        assert_eq!(output[1]["result"], json!({}));
    }
}
