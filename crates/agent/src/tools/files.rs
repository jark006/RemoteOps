use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use regex::RegexBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use remote_ops_protocol::{
    APPLY_PATCH_MAX_FILE_BYTES, APPLY_PATCH_MAX_HUNKS, APPLY_PATCH_MAX_PATCH_BYTES,
};

use crate::error::{AgentError, AgentResult};
use crate::tools::timefmt::format_epoch_iso;

pub const READ_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const READ_FILE_LINES_MAX_BYTES: usize = 1024 * 1024;
pub const READ_FILE_LINES_MAX_LINES: u64 = 10_000;
pub const READ_FILE_LINES_DEFAULT_LINES: u64 = 200;
pub const TAIL_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const FILE_HASH_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const GREP_MAX_PATTERN_BYTES: usize = 4 * 1024;
pub const GREP_MAX_GLOB_BYTES: usize = 1024;
pub const GREP_MAX_RESULTS: usize = 1000;
pub const GREP_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const LIST_FILES_MAX_DEPTH: usize = 64;

const READ_FILE_LINES_MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const GREP_MAX_SCANNED_FILES: usize = 10_000;
const GREP_MAX_SCANNED_ENTRIES: usize = 100_000;
const GREP_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const GREP_MAX_DEPTH: usize = 64;
const GREP_MAX_TEXT_BYTES: usize = 1024;
const GREP_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const LIST_FILES_MAX_SCANNED_ENTRIES: usize = 100_000;
const LIST_FILES_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub fn read_text(path: &str, offset: u64, max_bytes: usize) -> AgentResult<Value> {
    if max_bytes > READ_TEXT_MAX_BYTES {
        return Err(AgentError::invalid(format!(
            "max_bytes must be in range 0..={READ_TEXT_MAX_BYTES}"
        )));
    }
    let mut file = File::open(path).map_err(|err| AgentError::io(format!("open {path}"), err))?;
    let size = file
        .metadata()
        .map_err(|err| AgentError::io(format!("stat {path}"), err))?
        .len();
    if offset > size {
        return Err(AgentError::invalid("offset exceeds file size"));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| AgentError::io(format!("seek {path}"), err))?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    file.take(max_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(|err| AgentError::io(format!("read {path}"), err))?;
    let next_offset = offset + bytes.len() as u64;
    let truncated = next_offset < size;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(json!({
        "text": text,
        "metadata": {"offset": offset, "bytes_read": bytes.len(), "next_offset": next_offset, "truncated": truncated}
    }))
}

pub fn read_file_lines(
    path: &str,
    start_line: u64,
    end_line: Option<u64>,
    max_bytes: usize,
) -> AgentResult<Value> {
    validate_path(path)?;
    if start_line == 0 {
        return Err(AgentError::invalid("start_line must be at least 1"));
    }
    let end_line = end_line.unwrap_or_else(|| {
        start_line
            .saturating_add(READ_FILE_LINES_DEFAULT_LINES)
            .saturating_sub(1)
    });
    if end_line < start_line {
        return Err(AgentError::invalid(
            "end_line must be greater than or equal to start_line",
        ));
    }
    if end_line - start_line >= READ_FILE_LINES_MAX_LINES {
        return Err(AgentError::invalid(format!(
            "line range must not exceed {READ_FILE_LINES_MAX_LINES} lines"
        )));
    }
    if max_bytes > READ_FILE_LINES_MAX_BYTES {
        return Err(AgentError::invalid(format!(
            "max_bytes must be in range 0..={READ_FILE_LINES_MAX_BYTES}"
        )));
    }

    let target = Path::new(path);
    let metadata =
        fs::symlink_metadata(target).map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    if !metadata.file_type().is_file() {
        return Err(AgentError::invalid(
            "path must be a regular file and not a symlink",
        ));
    }
    let file = File::open(target).map_err(|err| AgentError::io(format!("open {path}"), err))?;
    let size = file
        .metadata()
        .map_err(|err| AgentError::io(format!("stat {path}"), err))?
        .len();
    let mut reader = BufReader::new(file);
    let mut current_line = 1u64;
    let mut bytes_scanned = 0u64;
    let mut output = Vec::with_capacity(max_bytes.min(8192));
    let mut line = Vec::new();
    let mut lines_returned = 0u64;
    let mut next_line = None;
    let mut truncated = false;

    loop {
        let remaining_scan = READ_FILE_LINES_MAX_SCAN_BYTES
            .saturating_add(1)
            .saturating_sub(bytes_scanned);
        if remaining_scan == 0 {
            return Err(AgentError::invalid(format!(
                "line scan exceeds {READ_FILE_LINES_MAX_SCAN_BYTES} bytes"
            )));
        }
        line.clear();
        let read = (&mut reader)
            .take(remaining_scan)
            .read_until(b'\n', &mut line)
            .map_err(|err| AgentError::io(format!("read {path}"), err))?;
        if read == 0 {
            break;
        }
        bytes_scanned += read as u64;
        if bytes_scanned > READ_FILE_LINES_MAX_SCAN_BYTES {
            return Err(AgentError::invalid(format!(
                "line scan exceeds {READ_FILE_LINES_MAX_SCAN_BYTES} bytes"
            )));
        }

        if current_line >= start_line {
            if output.len().saturating_add(line.len()) > max_bytes {
                truncated = true;
                next_line = Some(current_line);
                break;
            }
            output.extend_from_slice(&line);
            lines_returned += 1;
        }
        if current_line == end_line {
            if bytes_scanned < size {
                truncated = true;
                next_line = current_line.checked_add(1);
            }
            break;
        }
        current_line = current_line.saturating_add(1);
    }

    let bytes_returned = output.len();
    let text = String::from_utf8(output)
        .map_err(|_| AgentError::invalid("requested lines must contain valid UTF-8 text"))?;
    Ok(json!({
        "text": text,
        "metadata": {
            "start_line": start_line,
            "end_line": end_line,
            "lines_returned": lines_returned,
            "bytes_returned": bytes_returned,
            "next_line": next_line,
            "truncated": truncated
        }
    }))
}

pub fn tail_text(path: &str, lines: usize, max_bytes: usize) -> AgentResult<Value> {
    if lines > 10_000 {
        return Err(AgentError::invalid("lines must be in range 0..=10000"));
    }
    if max_bytes > TAIL_TEXT_MAX_BYTES {
        return Err(AgentError::invalid(format!(
            "max_bytes must be in range 0..={TAIL_TEXT_MAX_BYTES}"
        )));
    }
    let mut file = File::open(path).map_err(|err| AgentError::io(format!("open {path}"), err))?;
    let size = file
        .metadata()
        .map_err(|err| AgentError::io(format!("stat {path}"), err))?
        .len();
    let scan = size.min(max_bytes as u64);
    file.seek(SeekFrom::Start(size - scan))
        .map_err(|err| AgentError::io(format!("seek {path}"), err))?;
    let mut bytes = Vec::with_capacity(scan as usize);
    file.read_to_end(&mut bytes)
        .map_err(|err| AgentError::io(format!("read {path}"), err))?;
    let text = String::from_utf8_lossy(&bytes);
    let parts: Vec<&str> = text.lines().collect();
    let start = parts.len().saturating_sub(lines);
    let selected = parts[start..].join("\n");
    let truncated = scan < size || start > 0;
    Ok(json!({
        "text": selected,
        "metadata": {"bytes_scanned": scan, "lines_returned": parts.len() - start, "truncated": truncated}
    }))
}

pub fn write_text(path: &str, content: &str) -> AgentResult<Value> {
    let target = Path::new(path);
    let mode = fs::metadata(target)
        .ok()
        .map(|metadata| existing_mode(&metadata));
    write_bytes_atomically(target, path, content.as_bytes(), mode)?;
    Ok(json!({"bytes_written": content.len()}))
}

pub fn apply_patch(path: &str, patch: &str, expected_sha256: Option<&str>) -> AgentResult<Value> {
    validate_patch_arguments(path, patch, expected_sha256)?;
    let target = Path::new(path);
    let path_metadata =
        fs::symlink_metadata(target).map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    if !path_metadata.file_type().is_file() {
        return Err(AgentError::invalid(
            "path must be a regular file and not a symlink",
        ));
    }
    if path_metadata.len() > APPLY_PATCH_MAX_FILE_BYTES {
        return Err(AgentError::invalid(format!(
            "file exceeds {APPLY_PATCH_MAX_FILE_BYTES} bytes"
        )));
    }

    let mut source = Vec::with_capacity(path_metadata.len() as usize);
    File::open(target)
        .and_then(|mut file| file.read_to_end(&mut source))
        .map_err(|err| AgentError::io(format!("read {path}"), err))?;
    if source.len() as u64 > APPLY_PATCH_MAX_FILE_BYTES {
        return Err(AgentError::invalid(format!(
            "file exceeds {APPLY_PATCH_MAX_FILE_BYTES} bytes"
        )));
    }

    let sha256_before = sha256_hex(&source);
    if expected_sha256.is_some_and(|expected| !expected.eq_ignore_ascii_case(&sha256_before)) {
        return Err(AgentError::invalid(format!(
            "expected_sha256 does not match current file (current: {sha256_before})"
        )));
    }

    let parsed = ParsedPatch::parse(path, patch)?;
    let (bom, body) = source
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .map_or((&[][..], source.as_slice()), |body| {
            (&[0xef, 0xbb, 0xbf][..], body)
        });
    let text = std::str::from_utf8(body)
        .map_err(|_| AgentError::invalid("file must contain valid UTF-8 text"))?;
    let (mut lines, default_ending, had_final_ending) = parse_text_lines(text);
    for (index, hunk) in parsed.hunks.iter().enumerate() {
        apply_hunk(&mut lines, hunk).map_err(|message| {
            AgentError::invalid(format!(
                "hunk {} could not be applied: {message}",
                index + 1
            ))
        })?;
    }
    normalize_line_endings(&mut lines, default_ending, had_final_ending);

    let mut output = Vec::with_capacity(source.len().saturating_add(patch.len()));
    output.extend_from_slice(bom);
    for line in &lines {
        output.extend_from_slice(line.text.as_bytes());
        output.extend_from_slice(line.ending.as_bytes());
    }
    if output.len() as u64 > APPLY_PATCH_MAX_FILE_BYTES {
        return Err(AgentError::invalid(format!(
            "patched file exceeds {APPLY_PATCH_MAX_FILE_BYTES} bytes"
        )));
    }

    let sha256_after = sha256_hex(&output);
    let mode = existing_mode(&path_metadata);
    write_bytes_atomically(target, path, &output, Some(mode))?;
    Ok(json!({
        "bytes_before": source.len(),
        "bytes_after": output.len(),
        "hunks_applied": parsed.hunks.len(),
        "sha256_before": sha256_before,
        "sha256_after": sha256_after
    }))
}

fn write_bytes_atomically(
    target: &Path,
    display_path: &str,
    content: &[u8],
    mode: Option<u32>,
) -> AgentResult<()> {
    let parent = target
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = NamedTempFile::new_in(parent)
        .map_err(|err| AgentError::io(format!("create temporary file for {display_path}"), err))?;
    temp.write_all(content)
        .and_then(|_| temp.flush())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|err| AgentError::io(format!("write temporary file for {display_path}"), err))?;
    if let Some(mode) = mode {
        set_mode(temp.path(), mode)?;
    }
    persist_replace(temp, target, true)
}

fn validate_patch_arguments(
    path: &str,
    patch: &str,
    expected_sha256: Option<&str>,
) -> AgentResult<()> {
    if path.is_empty() {
        return Err(AgentError::invalid("path must not be empty"));
    }
    if path.contains('\0') {
        return Err(AgentError::invalid("path must not contain NUL"));
    }
    if patch.is_empty() {
        return Err(AgentError::invalid("patch must not be empty"));
    }
    if patch.len() > APPLY_PATCH_MAX_PATCH_BYTES {
        return Err(AgentError::invalid(format!(
            "patch exceeds {APPLY_PATCH_MAX_PATCH_BYTES} bytes"
        )));
    }
    if let Some(expected) = expected_sha256
        && (expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(AgentError::invalid(
            "expected_sha256 must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ParsedPatch {
    hunks: Vec<Hunk>,
}

impl ParsedPatch {
    fn parse(expected_path: &str, patch: &str) -> AgentResult<Self> {
        let lines: Vec<&str> = patch
            .lines()
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
            return Err(AgentError::invalid(
                "patch must start with '*** Begin Patch' and end with '*** End Patch'",
            ));
        }
        let Some(declared_path) = lines
            .get(1)
            .and_then(|line| line.strip_prefix("*** Update File: "))
        else {
            return Err(AgentError::invalid(
                "patch must contain exactly one '*** Update File: PATH' header",
            ));
        };
        if declared_path != expected_path {
            return Err(AgentError::invalid(
                "patch Update File path must exactly match path",
            ));
        }

        let mut hunks = Vec::new();
        let mut index = 2;
        while index + 1 < lines.len() {
            if lines[index] != "@@" {
                return Err(AgentError::invalid(format!(
                    "expected '@@' before patch line {}",
                    index + 1
                )));
            }
            index += 1;
            let mut operations = Vec::new();
            let mut has_change = false;
            while index + 1 < lines.len() && lines[index] != "@@" {
                let line = lines[index];
                let (kind, text) = match line.as_bytes().first() {
                    Some(b' ') => (HunkLineKind::Context, &line[1..]),
                    Some(b'-') => {
                        has_change = true;
                        (HunkLineKind::Remove, &line[1..])
                    }
                    Some(b'+') => {
                        has_change = true;
                        (HunkLineKind::Add, &line[1..])
                    }
                    _ => {
                        return Err(AgentError::invalid(format!(
                            "patch line {} must start with space, '+', or '-'",
                            index + 1
                        )));
                    }
                };
                operations.push(HunkLine {
                    kind,
                    text: text.to_string(),
                });
                index += 1;
            }
            if operations.is_empty() || !has_change {
                return Err(AgentError::invalid(
                    "each hunk must contain lines and at least one change",
                ));
            }
            if operations.iter().all(|line| line.kind == HunkLineKind::Add) {
                return Err(AgentError::invalid(
                    "addition-only hunks require at least one context line",
                ));
            }
            hunks.push(Hunk { operations });
            if hunks.len() > APPLY_PATCH_MAX_HUNKS {
                return Err(AgentError::invalid(format!(
                    "patch exceeds {APPLY_PATCH_MAX_HUNKS} hunks"
                )));
            }
        }
        if hunks.is_empty() {
            return Err(AgentError::invalid("patch must contain at least one hunk"));
        }
        Ok(Self { hunks })
    }
}

#[derive(Debug)]
struct Hunk {
    operations: Vec<HunkLine>,
}

#[derive(Debug)]
struct HunkLine {
    kind: HunkLineKind,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkLineKind {
    Context,
    Remove,
    Add,
}

#[derive(Debug, Clone)]
struct TextLine {
    text: String,
    ending: String,
}

fn parse_text_lines(text: &str) -> (Vec<TextLine>, &str, bool) {
    let mut lines = Vec::new();
    let mut start = 0;
    for (newline, _) in text.match_indices('\n') {
        let (line_end, ending) = if newline > start && text.as_bytes()[newline - 1] == b'\r' {
            (newline - 1, "\r\n")
        } else {
            (newline, "\n")
        };
        lines.push(TextLine {
            text: text[start..line_end].to_string(),
            ending: ending.to_string(),
        });
        start = newline + 1;
    }
    if start < text.len() {
        lines.push(TextLine {
            text: text[start..].to_string(),
            ending: String::new(),
        });
    }
    let default_ending = lines
        .iter()
        .find(|line| !line.ending.is_empty())
        .map(|line| line.ending.as_str())
        .unwrap_or("\n");
    let default_ending = if default_ending == "\r\n" {
        "\r\n"
    } else {
        "\n"
    };
    (lines, default_ending, text.ends_with('\n'))
}

fn apply_hunk(lines: &mut Vec<TextLine>, hunk: &Hunk) -> Result<(), &'static str> {
    let old_lines: Vec<&str> = hunk
        .operations
        .iter()
        .filter(|line| line.kind != HunkLineKind::Add)
        .map(|line| line.text.as_str())
        .collect();
    let matches: Vec<usize> = lines
        .windows(old_lines.len())
        .enumerate()
        .filter(|(_, window)| {
            window
                .iter()
                .map(|line| line.text.as_str())
                .eq(old_lines.iter().copied())
        })
        .map(|(index, _)| index)
        .take(2)
        .collect();
    let start = match matches.as_slice() {
        [] => return Err("old text was not found"),
        [start] => *start,
        _ => return Err("old text is ambiguous; add more context"),
    };

    let mut cursor = start;
    let mut replacement = Vec::new();
    for operation in &hunk.operations {
        match operation.kind {
            HunkLineKind::Context => {
                replacement.push(lines[cursor].clone());
                cursor += 1;
            }
            HunkLineKind::Remove => cursor += 1,
            HunkLineKind::Add => replacement.push(TextLine {
                text: operation.text.clone(),
                ending: String::new(),
            }),
        }
    }
    lines.splice(start..start + old_lines.len(), replacement);
    Ok(())
}

fn normalize_line_endings(lines: &mut [TextLine], default_ending: &str, had_final_ending: bool) {
    let Some((last, preceding)) = lines.split_last_mut() else {
        return;
    };
    for line in preceding {
        if line.ending.is_empty() {
            line.ending.push_str(default_ending);
        }
    }
    if had_final_ending {
        if last.ending.is_empty() {
            last.ending.push_str(default_ending);
        }
    } else {
        last.ending.clear();
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn grep(
    path: &str,
    pattern: &str,
    glob: Option<&str>,
    case_sensitive: bool,
    max_results: usize,
    max_file_bytes: u64,
) -> AgentResult<Value> {
    validate_path(path)?;
    if pattern.is_empty() {
        return Err(AgentError::invalid("pattern must not be empty"));
    }
    if pattern.len() > GREP_MAX_PATTERN_BYTES {
        return Err(AgentError::invalid(format!(
            "pattern exceeds {GREP_MAX_PATTERN_BYTES} bytes"
        )));
    }
    if max_results == 0 || max_results > GREP_MAX_RESULTS {
        return Err(AgentError::invalid(format!(
            "max_results must be in range 1..={GREP_MAX_RESULTS}"
        )));
    }
    if max_file_bytes == 0 || max_file_bytes > GREP_MAX_FILE_BYTES {
        return Err(AgentError::invalid(format!(
            "max_file_bytes must be in range 1..={GREP_MAX_FILE_BYTES}"
        )));
    }
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|err| AgentError::invalid(format!("invalid regex pattern: {err}")))?;
    let glob = compile_glob(glob)?;
    let root = Path::new(path);
    let (files, traversal_truncated) = collect_grep_files(root, glob.as_ref())?;
    let mut matches = Vec::new();
    let mut files_scanned = 0usize;
    let mut files_skipped = 0usize;
    let mut bytes_scanned = 0u64;
    let mut output_bytes = 256usize;
    let mut truncated = traversal_truncated;

    'files: for candidate in files {
        let metadata = fs::symlink_metadata(&candidate.path)
            .map_err(|err| AgentError::io(format!("stat {}", candidate.path.display()), err))?;
        if !metadata.file_type().is_file() || metadata.len() > max_file_bytes {
            files_skipped += 1;
            continue;
        }
        if bytes_scanned.saturating_add(metadata.len()) > GREP_MAX_TOTAL_BYTES {
            truncated = true;
            break;
        }
        let mut bytes = Vec::with_capacity((metadata.len() as usize).min(8192));
        File::open(&candidate.path)
            .and_then(|file| {
                file.take(max_file_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
            })
            .map_err(|err| AgentError::io(format!("read {}", candidate.path.display()), err))?;
        if bytes.len() as u64 > max_file_bytes {
            files_skipped += 1;
            continue;
        }
        files_scanned += 1;
        bytes_scanned += bytes.len() as u64;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            files_skipped += 1;
            continue;
        };

        for (line_index, line) in text.lines().enumerate() {
            let Some(found) = regex.find(line) else {
                continue;
            };
            if matches.len() == max_results {
                truncated = true;
                break 'files;
            }
            let (display_text, text_truncated) = truncate_utf8(line, GREP_MAX_TEXT_BYTES);
            let matched_line = json!({
                "path": candidate.relative_path,
                "line": line_index + 1,
                "column": found.start() + 1,
                "text": display_text,
                "text_truncated": text_truncated
            });
            let encoded_bytes = serde_json::to_vec(&matched_line)
                .map_err(|err| AgentError::command(format!("encode grep result: {err}")))?
                .len()
                .saturating_add(1);
            if output_bytes.saturating_add(encoded_bytes) > GREP_MAX_OUTPUT_BYTES {
                truncated = true;
                break 'files;
            }
            output_bytes += encoded_bytes;
            matches.push(matched_line);
        }
    }

    Ok(json!({
        "matches": matches,
        "files_scanned": files_scanned,
        "files_skipped": files_skipped,
        "bytes_scanned": bytes_scanned,
        "truncated": truncated
    }))
}

pub fn list_files(
    path: &str,
    cursor: Option<&str>,
    limit: usize,
    recursive: bool,
    pattern: Option<&str>,
    max_depth: usize,
) -> AgentResult<Value> {
    validate_path(path)?;
    if limit == 0 || limit > 1000 {
        return Err(AgentError::invalid("limit must be in range 1..=1000"));
    }
    if max_depth == 0 || max_depth > LIST_FILES_MAX_DEPTH {
        return Err(AgentError::invalid(format!(
            "max_depth must be in range 1..={LIST_FILES_MAX_DEPTH}"
        )));
    }
    if cursor.is_some_and(|value| value.contains('\0')) {
        return Err(AgentError::invalid("cursor must not contain NUL"));
    }
    let matcher = compile_glob(pattern)?;
    let root = Path::new(path);
    let metadata =
        fs::symlink_metadata(root).map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    if !metadata.file_type().is_dir() {
        return Err(AgentError::invalid(
            "path must be a directory and not a symlink",
        ));
    }
    let mut entries = Vec::new();
    let mut scanned = 0usize;
    collect_list_entries(
        root,
        root,
        1,
        recursive,
        max_depth,
        matcher.as_ref(),
        &mut scanned,
        &mut entries,
    )?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            cursor
                .map(|value| entry.name.as_str() > value)
                .unwrap_or(true)
        })
        .collect();
    let total = filtered.len();
    let mut output_bytes = 128usize;
    let mut page = Vec::new();
    for entry in filtered {
        if page.len() == limit {
            break;
        }
        let listed_entry = json!({
            "name": entry.name,
            "kind": entry.kind,
            "size": entry.size,
            "mtime": entry.mtime,
            "mtime_iso": format_epoch_iso(entry.mtime),
            "mode_str": ls_mode_string(entry.mode),
        });
        let encoded_bytes = serde_json::to_vec(&listed_entry)
            .map_err(|err| AgentError::command(format!("encode list result: {err}")))?
            .len()
            .saturating_add(1);
        if output_bytes.saturating_add(encoded_bytes) > LIST_FILES_MAX_OUTPUT_BYTES {
            break;
        }
        output_bytes += encoded_bytes;
        page.push(listed_entry);
    }
    let truncated = page.len() < total;
    let next_cursor = if truncated {
        page.last()
            .and_then(|entry| entry["name"].as_str())
            .map(str::to_string)
    } else {
        None
    };
    Ok(json!({"entries": page, "next_cursor": next_cursor, "truncated": truncated}))
}

struct GrepCandidate {
    path: PathBuf,
    relative_path: String,
}

struct ListEntry {
    name: String,
    kind: &'static str,
    size: u64,
    mtime: i64,
    mode: u32,
}

fn collect_grep_files(
    root: &Path,
    matcher: Option<&GlobMatcher>,
) -> AgentResult<(Vec<GrepCandidate>, bool)> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|err| AgentError::io(format!("stat {}", root.display()), err))?;
    if metadata.file_type().is_file() {
        let relative_path = root
            .file_name()
            .unwrap_or(root.as_os_str())
            .to_string_lossy()
            .into_owned();
        if matcher.is_none_or(|value| value.is_match(&relative_path)) {
            return Ok((
                vec![GrepCandidate {
                    path: root.to_path_buf(),
                    relative_path,
                }],
                false,
            ));
        }
        return Ok((Vec::new(), false));
    }
    if !metadata.file_type().is_dir() {
        return Err(AgentError::invalid(
            "path must be a regular file or directory and not a symlink",
        ));
    }
    let mut files = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_entries = 0usize;
    let truncated = collect_grep_directory(
        root,
        root,
        0,
        matcher,
        &mut scanned_entries,
        &mut scanned_files,
        &mut files,
    )?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok((files, truncated))
}

fn collect_grep_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    matcher: Option<&GlobMatcher>,
    scanned_entries: &mut usize,
    scanned_files: &mut usize,
    files: &mut Vec<GrepCandidate>,
) -> AgentResult<bool> {
    let remaining_entries = GREP_MAX_SCANNED_ENTRIES.saturating_sub(*scanned_entries);
    let (directory_entries, directory_truncated) =
        sorted_directory_entries(directory, remaining_entries)?;
    *scanned_entries += directory_entries.len();
    let mut truncated = directory_truncated;
    for entry in directory_entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| AgentError::io(format!("stat {}", path.display()), err))?;
        if metadata.file_type().is_dir() {
            if !is_ignored_search_directory(&entry.file_name()) {
                if depth == GREP_MAX_DEPTH {
                    truncated = true;
                } else if collect_grep_directory(
                    root,
                    &path,
                    depth + 1,
                    matcher,
                    scanned_entries,
                    scanned_files,
                    files,
                )? {
                    return Ok(true);
                }
            }
        } else if metadata.file_type().is_file() {
            if *scanned_files == GREP_MAX_SCANNED_FILES {
                return Ok(true);
            }
            *scanned_files += 1;
            let relative_path = relative_display_path(root, &path);
            if matcher.is_none_or(|value| value.is_match(&relative_path)) {
                files.push(GrepCandidate {
                    path,
                    relative_path,
                });
            }
        }
    }
    Ok(truncated)
}

#[allow(clippy::too_many_arguments)]
fn collect_list_entries(
    root: &Path,
    directory: &Path,
    depth: usize,
    recursive: bool,
    max_depth: usize,
    matcher: Option<&GlobMatcher>,
    scanned: &mut usize,
    entries: &mut Vec<ListEntry>,
) -> AgentResult<()> {
    let remaining_entries = LIST_FILES_MAX_SCANNED_ENTRIES.saturating_sub(*scanned);
    let (directory_entries, directory_truncated) =
        sorted_directory_entries(directory, remaining_entries)?;
    if directory_truncated {
        return Err(AgentError::invalid(format!(
            "directory traversal exceeds {LIST_FILES_MAX_SCANNED_ENTRIES} entries"
        )));
    }
    *scanned += directory_entries.len();
    for entry in directory_entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| AgentError::io(format!("stat {}", path.display()), err))?;
        let name = relative_display_path(root, &path);
        if matcher.is_none_or(|value| value.is_match(&name)) {
            entries.push(ListEntry {
                name,
                kind: file_kind(&metadata),
                size: metadata.len(),
                mtime: metadata_mtime(&metadata),
                mode: existing_mode(&metadata),
            });
        }
        if recursive && depth < max_depth && metadata.file_type().is_dir() {
            collect_list_entries(
                root,
                &path,
                depth + 1,
                recursive,
                max_depth,
                matcher,
                scanned,
                entries,
            )?;
        }
    }
    Ok(())
}

fn sorted_directory_entries(path: &Path, limit: usize) -> AgentResult<(Vec<fs::DirEntry>, bool)> {
    let mut entries = Vec::with_capacity(limit.min(1024));
    let directory = fs::read_dir(path)
        .map_err(|err| AgentError::io(format!("list {}", path.display()), err))?;
    let mut truncated = false;
    for entry in directory {
        if entries.len() == limit {
            truncated = true;
            break;
        }
        entries.push(entry.map_err(|err| AgentError::io(format!("list {}", path.display()), err))?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok((entries, truncated))
}

fn compile_glob(pattern: Option<&str>) -> AgentResult<Option<GlobMatcher>> {
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    if pattern.is_empty() {
        return Err(AgentError::invalid("glob pattern must not be empty"));
    }
    if pattern.len() > GREP_MAX_GLOB_BYTES {
        return Err(AgentError::invalid(format!(
            "glob pattern exceeds {GREP_MAX_GLOB_BYTES} bytes"
        )));
    }
    Glob::new(pattern)
        .map(|glob| Some(glob.compile_matcher()))
        .map_err(|err| AgentError::invalid(format!("invalid glob pattern: {err}")))
}

fn relative_display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_ignored_search_directory(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    [
        ".git",
        ".hg",
        ".svn",
        ".next",
        "node_modules",
        "target",
        "dist",
        "build",
    ]
    .iter()
    .any(|ignored| name.eq_ignore_ascii_case(ignored))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (&str, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (&value[..end], true)
}

fn validate_path(path: &str) -> AgentResult<()> {
    if path.is_empty() {
        return Err(AgentError::invalid("path must not be empty"));
    }
    if path.contains('\0') {
        return Err(AgentError::invalid("path must not contain NUL"));
    }
    Ok(())
}

pub fn stat(path: &str) -> AgentResult<Value> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    let mtime = metadata_mtime(&metadata);
    let mode = existing_mode(&metadata);
    Ok(json!({
        "size": metadata.len(),
        "mtime": mtime,
        "mtime_iso": format_epoch_iso(mtime),
        "mode": mode,
        "mode_str": ls_mode_string(mode),
        "kind": file_kind(&metadata),
    }))
}

fn metadata_mtime(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

pub fn file_hash(path: &str, max_bytes: u64) -> AgentResult<Value> {
    if max_bytes > FILE_HASH_MAX_BYTES {
        return Err(AgentError::invalid(format!(
            "max_bytes must be in range 0..={FILE_HASH_MAX_BYTES}"
        )));
    }
    let mut file = File::open(path).map_err(|err| AgentError::io(format!("open {path}"), err))?;
    let metadata = file
        .metadata()
        .map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    if !metadata.is_file() {
        return Err(AgentError::invalid("path must be a regular file"));
    }
    if metadata.len() > max_bytes {
        return Err(AgentError::invalid(format!(
            "file exceeds max_bytes ({max_bytes})"
        )));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| AgentError::io(format!("read {path}"), err))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        total += read as u64;
    }
    Ok(
        json!({"algorithm": "sha256", "digest": format!("{:x}", digest.finalize()), "bytes_hashed": total}),
    )
}

pub fn create_transfer_temp(target: &Path) -> AgentResult<NamedTempFile> {
    let parent = target
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    NamedTempFile::new_in(parent).map_err(|err| {
        AgentError::io(
            format!("create transfer temporary file for {}", target.display()),
            err,
        )
    })
}

pub fn persist_replace(temp: NamedTempFile, target: &Path, overwrite: bool) -> AgentResult<()> {
    if target.exists() && !overwrite {
        return Err(AgentError::invalid(format!(
            "destination already exists: {}",
            target.display()
        )));
    }
    platform_persist(temp, target)?;
    sync_parent_directory(target)
}

pub fn persist_path_replace(source: &Path, target: &Path, overwrite: bool) -> AgentResult<()> {
    if target.exists() && !overwrite {
        return Err(AgentError::invalid(format!(
            "destination already exists: {}",
            target.display()
        )));
    }
    platform_persist_path(source, target)?;
    sync_parent_directory(target)
}

pub fn preserve_existing_mode(target: &Path, temporary: &Path) -> AgentResult<()> {
    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(target) {
        set_mode(temporary, existing_mode(&metadata))?;
    }
    #[cfg(not(unix))]
    let _ = (target, temporary);
    Ok(())
}

#[cfg(not(windows))]
fn platform_persist(temp: NamedTempFile, target: &Path) -> AgentResult<()> {
    temp.persist(target)
        .map(|_| ())
        .map_err(|error| AgentError::io(format!("persist {}", target.display()), error.error))
}

#[cfg(not(windows))]
fn platform_persist_path(source: &Path, target: &Path) -> AgentResult<()> {
    fs::rename(source, target)
        .map_err(|error| AgentError::io(format!("persist {}", target.display()), error))
}

#[cfg(unix)]
fn sync_parent_directory(target: &Path) -> AgentResult<()> {
    let parent = target.parent().unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AgentError::io(format!("sync directory {}", parent.display()), error))
}

#[cfg(not(unix))]
fn sync_parent_directory(_target: &Path) -> AgentResult<()> {
    Ok(())
}

#[cfg(windows)]
fn platform_persist(temp: NamedTempFile, target: &Path) -> AgentResult<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let temporary = temp.into_temp_path();
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(AgentError::io(
            format!("persist {}", target.display()),
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn platform_persist_path(source: &Path, target: &Path) -> AgentResult<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(AgentError::io(
            format!("persist {}", target.display()),
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

fn file_kind(metadata: &fs::Metadata) -> &'static str {
    let kind = metadata.file_type();
    if kind.is_file() {
        "file"
    } else if kind.is_dir() {
        "dir"
    } else if kind.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

/// Render a raw Unix `st_mode` value as the 10-character string used by `ls -l`
/// (for example `-rwxr-xr-x`, `drwxr-x---`, `lrwxrwxrwx`). setuid/setgid/sticky
/// overwrite the execute slot with `s`/`S`/`t`/`T` exactly as `ls` does.
fn ls_mode_string(mode: u32) -> String {
    let file_type = match mode & 0o170000 {
        0o140000 => 's', // socket
        0o120000 => 'l', // symlink
        0o100000 => '-', // regular file
        0o060000 => 'b', // block device
        0o040000 => 'd', // directory
        0o020000 => 'c', // character device
        0o010000 => 'p', // fifo
        _ => '-',
    };
    let mut text = String::with_capacity(10);
    text.push(file_type);
    let permissions = mode & 0o777;
    let setid = [0o4000u32, 0o2000u32, 0o1000u32];
    for (index, override_bits) in setid.iter().enumerate() {
        let shift = 6 - 3 * index as u32;
        let group = (permissions >> shift) & 0o7;
        for (bit, ch) in [(0o4u32, 'r'), (0o2u32, 'w'), (0o1u32, 'x')] {
            text.push(if group & bit != 0 { ch } else { '-' });
        }
        if mode & override_bits != 0 {
            let exec = group & 0o1 != 0;
            text.pop();
            text.push(if index < 2 {
                if exec { 's' } else { 'S' }
            } else if exec {
                't'
            } else {
                'T'
            });
        }
    }
    text
}

#[cfg(unix)]
fn existing_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn existing_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> AgentResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| AgentError::io(format!("set mode on {}", path.display()), err))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> AgentResult<()> {
    Ok(())
}

pub fn ensure_regular_file(path: &Path) -> AgentResult<(File, u64)> {
    let path_metadata = fs::symlink_metadata(path)
        .map_err(|err| AgentError::io(format!("stat {}", path.display()), err))?;
    if !path_metadata.file_type().is_file() {
        return Err(AgentError::invalid(
            "source must be a regular file and not a symlink",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|err| AgentError::io(format!("open {}", path.display()), err))?;
    let metadata = file
        .metadata()
        .map_err(|err| AgentError::io(format!("stat {}", path.display()), err))?;
    if !metadata.is_file() {
        return Err(AgentError::invalid("source must be a regular file"));
    }
    Ok((file, metadata.len()))
}

pub fn normalized_path(path: &str) -> AgentResult<PathBuf> {
    if path.is_empty() {
        Err(AgentError::invalid("path must not be empty"))
    } else {
        Ok(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_patch(path: &str, hunks: &str) -> String {
        format!("*** Begin Patch\n*** Update File: {path}\n{hunks}*** End Patch")
    }

    #[test]
    fn reads_utf8_line_ranges_without_splitting_characters() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unicode.txt");
        fs::write(&path, "one\n二\nthree\nfour").unwrap();

        let result = read_file_lines(&path.to_string_lossy(), 2, Some(3), 1024).unwrap();

        assert_eq!(result["text"], "二\nthree\n");
        assert_eq!(result["metadata"]["lines_returned"], 2);
        assert_eq!(result["metadata"]["next_line"], 4);
        assert_eq!(result["metadata"]["truncated"], true);
    }

    #[test]
    fn line_reader_stops_before_exceeding_output_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bounded.txt");
        fs::write(&path, "short\na line that does not fit\nlast\n").unwrap();

        let result = read_file_lines(&path.to_string_lossy(), 1, Some(3), 8).unwrap();

        assert_eq!(result["text"], "short\n");
        assert_eq!(result["metadata"]["lines_returned"], 1);
        assert_eq!(result["metadata"]["next_line"], 2);
        assert_eq!(result["metadata"]["truncated"], true);
    }

    #[test]
    fn grep_filters_paths_and_skips_generated_directories() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("src");
        let generated = directory.path().join("node_modules");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&generated).unwrap();
        fs::write(
            source.join("main.rs"),
            "fn main() {}\nneedle here\nNEEDLE again\n",
        )
        .unwrap();
        fs::write(source.join("notes.txt"), "needle in excluded file\n").unwrap();
        fs::write(generated.join("ignored.rs"), "needle in generated file\n").unwrap();

        let result = grep(
            &directory.path().to_string_lossy(),
            "needle",
            Some("src/*.rs"),
            false,
            10,
            1024,
        )
        .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|item| item["path"] == "src/main.rs"));
        assert_eq!(matches[0]["line"], 2);
        assert_eq!(matches[1]["line"], 3);
        assert_eq!(result["files_scanned"], 1);
        assert_eq!(result["truncated"], false);
    }

    #[test]
    fn grep_reports_result_limit_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("many.txt");
        fs::write(&path, "match\nmatch\nmatch\n").unwrap();

        let result = grep(&path.to_string_lossy(), "match", None, true, 2, 1024).unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 2);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn list_files_recurses_filters_and_paginates_relative_paths() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src/nested")).unwrap();
        fs::write(directory.path().join("root.txt"), "root").unwrap();
        fs::write(directory.path().join("src/lib.rs"), "lib").unwrap();
        fs::write(directory.path().join("src/nested/mod.rs"), "mod").unwrap();

        let first = list_files(
            &directory.path().to_string_lossy(),
            None,
            1,
            true,
            Some("*.rs"),
            3,
        )
        .unwrap();
        assert_eq!(first["entries"][0]["name"], "src/lib.rs");
        assert_eq!(first["next_cursor"], "src/lib.rs");
        assert_eq!(first["truncated"], true);

        let second = list_files(
            &directory.path().to_string_lossy(),
            Some("src/lib.rs"),
            10,
            true,
            Some("*.rs"),
            3,
        )
        .unwrap();
        assert_eq!(second["entries"][0]["name"], "src/nested/mod.rs");
        assert_eq!(second["truncated"], false);
    }

    #[test]
    fn list_files_respects_maximum_depth() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("one/two")).unwrap();
        fs::write(directory.path().join("one/top.txt"), "top").unwrap();
        fs::write(directory.path().join("one/two/deep.txt"), "deep").unwrap();

        let result = list_files(
            &directory.path().to_string_lossy(),
            None,
            100,
            true,
            None,
            2,
        )
        .unwrap();
        let names = result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert!(names.contains(&"one/top.txt"));
        assert!(!names.contains(&"one/two/deep.txt"));
    }

    #[test]
    fn applies_multiple_context_checked_hunks_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.txt");
        let path_string = path.to_string_lossy();
        fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let patch = update_patch(
            &path_string,
            "@@\n alpha\n-beta\n+BETA\n@@\n gamma\n-delta\n+DELTA\n",
        );

        let result = apply_patch(&path_string, &patch, None).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "alpha\nBETA\ngamma\nDELTA\n"
        );
        assert_eq!(result["hunks_applied"], 2);
        assert_eq!(result["bytes_before"], 23);
        assert_eq!(result["bytes_after"], 23);
        assert_ne!(result["sha256_before"], result["sha256_after"]);
    }

    #[test]
    fn preserves_bom_crlf_and_missing_final_newline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("windows.txt");
        let path_string = path.to_string_lossy();
        fs::write(&path, b"\xef\xbb\xbfone\r\ntwo").unwrap();
        let patch = update_patch(&path_string, "@@\n one\n-two\n+changed\n");

        apply_patch(&path_string, &patch, None).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"\xef\xbb\xbfone\r\nchanged");
    }

    #[test]
    fn rejects_ambiguous_context_without_changing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("repeated.txt");
        let path_string = path.to_string_lossy();
        let original = "same\nold\nsame\nold\n";
        fs::write(&path, original).unwrap();
        let patch = update_patch(&path_string, "@@\n same\n-old\n+new\n");

        let error = apply_patch(&path_string, &patch, None).unwrap_err();

        assert!(error.message.contains("ambiguous"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn expected_hash_conflict_does_not_change_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conflict.txt");
        let path_string = path.to_string_lossy();
        fs::write(&path, "before\n").unwrap();
        let patch = update_patch(&path_string, "@@\n-before\n+after\n");

        let error = apply_patch(&path_string, &patch, Some(&"0".repeat(64))).unwrap_err();

        assert!(error.message.contains("does not match"));
        assert_eq!(fs::read_to_string(path).unwrap(), "before\n");
    }

    #[test]
    fn rejects_header_path_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.txt");
        let path_string = path.to_string_lossy();
        fs::write(&path, "before\n").unwrap();
        let patch = update_patch("different.txt", "@@\n-before\n+after\n");

        let error = apply_patch(&path_string, &patch, None).unwrap_err();

        assert!(error.message.contains("exactly match"));
        assert_eq!(fs::read_to_string(path).unwrap(), "before\n");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.txt");
        let link = directory.path().join("link.txt");
        let link_string = link.to_string_lossy();
        fs::write(&target, "before\n").unwrap();
        symlink(&target, &link).unwrap();
        let patch = update_patch(&link_string, "@@\n-before\n+after\n");

        let error = apply_patch(&link_string, &patch, None).unwrap_err();

        assert!(error.message.contains("not a symlink"));
        assert_eq!(fs::read_to_string(target).unwrap(), "before\n");
    }

    #[test]
    fn ls_mode_string_formats_type_permissions_and_special_bits() {
        assert_eq!(ls_mode_string(0o100755), "-rwxr-xr-x");
        assert_eq!(ls_mode_string(0o40750), "drwxr-x---");
        assert_eq!(ls_mode_string(0o120777), "lrwxrwxrwx");
        assert_eq!(ls_mode_string(0o010600), "prw-------");
        assert_eq!(ls_mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(ls_mode_string(0o102644), "-rw-r-Sr--");
        assert_eq!(ls_mode_string(0o101777), "-rwxrwxrwt");
        assert_eq!(ls_mode_string(0o101744), "-rwxr--r-T");
        assert_eq!(ls_mode_string(0), "----------");
    }

    #[test]
    fn stat_result_includes_derived_display_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        fs::write(&path, b"payload").unwrap();

        let result = stat(&path.to_string_lossy()).unwrap();

        assert_eq!(result["size"], 7);
        assert_eq!(result["kind"], "file");
        let mtime = result["mtime"].as_i64().unwrap();
        assert_eq!(
            result["mtime_iso"].as_str().unwrap(),
            format_epoch_iso(mtime)
        );
        let mode_str = result["mode_str"].as_str().unwrap();
        assert_eq!(mode_str.len(), 10);
        #[cfg(unix)]
        assert!(mode_str.starts_with('-'));
        #[cfg(not(unix))]
        assert_eq!(mode_str, "----------");
    }

    #[test]
    fn list_files_entries_carry_time_and_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        fs::write(&path, b"payload").unwrap();

        let result = list_files(
            &directory.path().to_string_lossy(),
            None,
            10,
            false,
            None,
            1,
        )
        .unwrap();
        let entry = &result["entries"][0];
        assert_eq!(entry["name"], "sample.txt");
        assert_eq!(entry["size"], 7);
        let mtime = entry["mtime"].as_i64().unwrap();
        assert_eq!(
            entry["mtime_iso"].as_str().unwrap(),
            format_epoch_iso(mtime)
        );
        assert_eq!(entry["mode_str"].as_str().unwrap().len(), 10);
    }
}
