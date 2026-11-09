//! Limits apply to encoded JSON bytes, including escaping and lossy UTF-8.
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug)]
pub enum JsonEncodeError {
    TooLarge(usize),
    Json(serde_json::Error),
}

impl std::fmt::Display for JsonEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(limit) => write!(f, "encoded JSON exceeds {limit} bytes"),
            Self::Json(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for JsonEncodeError {}

pub fn to_vec_bounded<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, JsonEncodeError> {
    encode(value, limit, false)
}

pub fn to_vec_pretty_bounded<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, JsonEncodeError> {
    encode(value, limit, true)
}

fn encode<T: Serialize>(value: &T, limit: usize, pretty: bool) -> Result<Vec<u8>, JsonEncodeError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
        exceeded: bool,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(io::Error::other("JSON byte limit exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::with_capacity(limit.min(8192)),
        limit,
        exceeded: false,
    };
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    match result {
        Ok(()) => Ok(writer.bytes),
        Err(_) if writer.exceeded => Err(JsonEncodeError::TooLarge(limit)),
        Err(error) => Err(JsonEncodeError::Json(error)),
    }
}

pub fn json_char_bytes(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\0'..='\u{1f}' => 6,
        _ => character.len_utf8(),
    }
}

pub fn json_text_bytes(text: &str) -> usize {
    text.chars().map(json_char_bytes).sum()
}

/// Largest raw-byte prefix whose lossy UTF-8 representation fits the budget.
pub fn lossy_prefix_len(bytes: &[u8], budget: usize) -> usize {
    let mut consumed = 0;
    let mut remaining = budget;
    while consumed < bytes.len() {
        let rest = &bytes[consumed..];
        let (valid, invalid) = match std::str::from_utf8(rest) {
            Ok(text) => (text, None),
            Err(error) => (
                std::str::from_utf8(&rest[..error.valid_up_to()]).expect("valid prefix"),
                Some(
                    error
                        .error_len()
                        .unwrap_or(rest.len() - error.valid_up_to()),
                ),
            ),
        };
        for character in valid.chars() {
            let cost = json_char_bytes(character);
            if cost > remaining {
                return consumed;
            }
            remaining -= cost;
            consumed += character.len_utf8();
        }
        if let Some(length) = invalid {
            if remaining < 3 {
                return consumed;
            }
            remaining -= 3;
            consumed += length;
        }
    }
    consumed
}

pub fn json_suffix_start(text: &str, budget: usize) -> usize {
    let mut start = text.len();
    let mut remaining = budget;
    for (offset, character) in text.char_indices().rev() {
        let cost = json_char_bytes(character);
        if cost > remaining {
            break;
        }
        remaining -= cost;
        start = offset;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_limits_count_escapes_and_invalid_utf8() {
        for bytes in [
            b"a\0\xffb".as_slice(),
            "中文\n".as_bytes(),
            b"\xf0\x9f\x92".as_slice(),
            b"\"\\".as_slice(),
        ] {
            for budget in 0..24 {
                let prefix = lossy_prefix_len(bytes, budget);
                let text = String::from_utf8_lossy(&bytes[..prefix]);
                assert!(json_text_bytes(&text) <= budget);
                assert_eq!(
                    serde_json::to_vec(&text).unwrap().len(),
                    json_text_bytes(&text) + 2
                );
            }
        }
        let text = "零\0\n尾";
        assert_eq!(&text[json_suffix_start(text, 3)..], "尾");
        assert!(matches!(
            to_vec_bounded(&"\0".repeat(10), 20),
            Err(JsonEncodeError::TooLarge(20))
        ));
        let value = serde_json::json!({"text":"中文\0"});
        let expected = serde_json::to_vec(&value).unwrap();
        assert_eq!(to_vec_bounded(&value, expected.len()).unwrap(), expected);
    }
}
