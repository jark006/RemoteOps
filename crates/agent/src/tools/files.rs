use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use remote_ops_protocol::{
    APPLY_PATCH_MAX_FILE_BYTES, APPLY_PATCH_MAX_HUNKS, APPLY_PATCH_MAX_PATCH_BYTES,
};

use crate::error::{AgentError, AgentResult};

pub const READ_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const TAIL_TEXT_MAX_BYTES: usize = 1024 * 1024;
pub const FILE_HASH_MAX_BYTES: u64 = 64 * 1024 * 1024;

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
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str("\n[truncated]");
    }
    Ok(json!({
        "text": text,
        "metadata": {"offset": offset, "bytes_read": bytes.len(), "next_offset": next_offset, "truncated": truncated}
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
    let mut selected = parts[start..].join("\n");
    let truncated = scan < size || start > 0;
    if truncated {
        selected.push_str("\n[truncated]");
    }
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

pub fn list_dir(path: &str, cursor: Option<&str>, limit: usize) -> AgentResult<Value> {
    if limit == 0 || limit > 1000 {
        return Err(AgentError::invalid("limit must be in range 1..=1000"));
    }
    let mut entries = fs::read_dir(path)
        .map_err(|err| AgentError::io(format!("list {path}"), err))?
        .map(|entry| entry.map_err(|err| AgentError::io(format!("list {path}"), err)))
        .collect::<AgentResult<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            cursor
                .map(|value| entry.file_name().to_string_lossy().as_ref() > value)
                .unwrap_or(true)
        })
        .collect();
    let truncated = filtered.len() > limit;
    let page = filtered
        .into_iter()
        .take(limit)
        .map(|entry| {
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|err| AgentError::io(format!("stat {}", entry.path().display()), err))?;
            Ok(json!({
                "name": entry.file_name().to_string_lossy(),
                "kind": file_kind(&metadata),
                "size": metadata.len()
            }))
        })
        .collect::<AgentResult<Vec<_>>>()?;
    let next_cursor = if truncated {
        page.last()
            .and_then(|entry| entry["name"].as_str())
            .map(str::to_string)
    } else {
        None
    };
    Ok(json!({"entries": page, "next_cursor": next_cursor, "truncated": truncated}))
}

pub fn stat(path: &str) -> AgentResult<Value> {
    let metadata =
        fs::symlink_metadata(path).map_err(|err| AgentError::io(format!("stat {path}"), err))?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    Ok(
        json!({"size": metadata.len(), "mtime": mtime, "mode": existing_mode(&metadata), "kind": file_kind(&metadata)}),
    )
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
}
