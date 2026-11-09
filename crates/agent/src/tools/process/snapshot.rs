use crate::error::{AgentError, AgentResult};
use serde_json::{Value, json};

pub(super) use remote_ops_protocol::arguments::{
    MAX_PROCESS_CMDLINE_BYTES as MAX_CMDLINE_BYTES, MAX_PROCESS_IDS,
    MAX_PROCESS_PAGE_BYTES as MAX_PAGE_BYTES, MAX_PROCESS_SCAN_BYTES as MAX_SCAN_BYTES,
    MAX_PROCESS_SCAN_COUNT as MAX_SCAN_PROCESSES,
};

#[derive(Default)]
pub(super) struct CommandLine {
    pub text: String,
    pub available: bool,
    pub truncated: bool,
    pub bytes_read: usize,
}

impl CommandLine {
    pub fn from_text(mut text: String, bytes_read: usize, truncated: bool) -> Self {
        let mut boundary = text.len().min(MAX_CMDLINE_BYTES);
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let truncated = truncated || boundary < text.len();
        text.truncate(boundary);
        Self {
            text,
            available: true,
            truncated,
            bytes_read,
        }
    }
}

pub(super) struct Entry {
    pub pid: u32,
    pub name: String,
    pub command: CommandLine,
}

pub(super) fn limit_error(message: impl Into<String>) -> AgentError {
    AgentError {
        kind: "resource_limit",
        message: message.into(),
    }
}

/// Use the last *examined* PID for scan-limited pages. Filtering may legitimately
/// produce an empty page with a continuation cursor.
pub(super) fn page(
    ids: impl IntoIterator<Item = u32>,
    after: u32,
    filter: Option<&str>,
    limit: usize,
    reserve: usize,
    mut inspect: impl FnMut(u32) -> Option<Entry>,
) -> AgentResult<Value> {
    let mut ids = ids.into_iter().filter(|pid| *pid > after).peekable();
    let mut processes = Vec::new();
    let mut output_bytes = 512;
    let mut scanned = 0;
    let mut bytes_read = 0usize;
    let mut last = after;
    let mut incomplete = false;
    let mut more = false;
    while let Some(pid) = ids.next() {
        if scanned == MAX_SCAN_PROCESSES || MAX_SCAN_BYTES.saturating_sub(bytes_read) < reserve {
            more = true;
            break;
        }
        scanned += 1;
        let Some(entry) = inspect(pid) else {
            if filter.is_some() {
                incomplete = true;
            }
            last = pid;
            continue;
        };
        bytes_read = bytes_read.saturating_add(entry.command.bytes_read);
        if filter.is_some() && entry.name.is_empty() {
            incomplete = true;
        }
        let name_matches = filter.is_some_and(|needle| entry.name.contains(needle));
        if filter.is_some()
            && !name_matches
            && (!entry.command.available || entry.command.truncated)
        {
            incomplete = true;
        }
        if filter.is_some_and(|needle| !name_matches && !entry.command.text.contains(needle)) {
            last = pid;
            continue;
        }
        let item = json!({"pid":entry.pid,"name":entry.name,"cmdline":entry.command.text,"cmdline_available":entry.command.available,"cmdline_truncated":entry.command.truncated});
        let cost = serde_json::to_vec(&item)
            .map_err(|error| AgentError::command(error.to_string()))?
            .len()
            + 1;
        if processes.len() == limit || output_bytes + cost > MAX_PAGE_BYTES {
            more = true;
            break;
        }
        last = pid;
        output_bytes += cost;
        processes.push(item);
        if processes.len() == limit && ids.peek().is_some() {
            more = true;
            break;
        }
    }
    let next_cursor = more.then(|| last.to_string());
    Ok(
        json!({"processes":processes,"next_cursor":next_cursor,"truncated":more,"scan":{"processes_scanned":scanned,"cmdline_bytes_read":bytes_read,"filter_incomplete":incomplete}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(pid: u32, text: String) -> Entry {
        Entry {
            pid,
            name: "fixture".into(),
            command: CommandLine::from_text(text, MAX_CMDLINE_BYTES, false),
        }
    }
    #[test]
    fn encoded_page_limit_preserves_the_unreturned_pid() {
        let mut after = 0;
        let mut actual = Vec::new();
        loop {
            let result = page(1..=12, after, None, 1024, MAX_CMDLINE_BYTES, |pid| {
                Some(entry(pid, "\u{1}".repeat(MAX_CMDLINE_BYTES)))
            })
            .unwrap();
            assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_PAGE_BYTES);
            actual.extend(
                result["processes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|process| process["pid"].as_u64().unwrap()),
            );
            let Some(cursor) = result["next_cursor"].as_str() else {
                break;
            };
            let next = cursor.parse().unwrap();
            assert!(next > after);
            after = next;
        }
        assert_eq!(actual, (1..=12).collect::<Vec<_>>());
    }
    #[test]
    fn filtered_empty_pages_make_progress_at_the_scan_limit() {
        let result = page(
            1..=1000,
            0,
            Some("absent"),
            1024,
            MAX_CMDLINE_BYTES,
            |pid| Some(entry(pid, "tiny".into())),
        )
        .unwrap();
        assert!(result["processes"].as_array().unwrap().is_empty());
        assert_eq!(result["next_cursor"], "128");
        assert_eq!(result["scan"]["cmdline_bytes_read"], MAX_SCAN_BYTES);
    }
    #[test]
    fn command_line_availability_and_truncation_are_explicit() {
        let command =
            CommandLine::from_text("界".repeat(MAX_CMDLINE_BYTES), MAX_CMDLINE_BYTES, false);
        assert!(command.truncated);
        assert!(command.text.len() <= MAX_CMDLINE_BYTES);
        let result = page([1], 0, Some("unseen"), 1, MAX_CMDLINE_BYTES, |pid| {
            Some(Entry {
                pid,
                name: "fixture".into(),
                command: CommandLine::default(),
            })
        })
        .unwrap();
        assert_eq!(result["scan"]["filter_incomplete"], true);
        let missing_name = page([1], 0, Some("unseen"), 1, MAX_CMDLINE_BYTES, |pid| {
            Some(Entry {
                pid,
                name: String::new(),
                command: CommandLine::from_text("visible".into(), 7, false),
            })
        })
        .unwrap();
        assert_eq!(missing_name["scan"]["filter_incomplete"], true);
    }
}
