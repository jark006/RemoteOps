use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

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
    let parent = target
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mode = fs::metadata(target)
        .ok()
        .map(|metadata| existing_mode(&metadata));
    let mut temp = NamedTempFile::new_in(parent)
        .map_err(|err| AgentError::io(format!("create temporary file for {path}"), err))?;
    temp.write_all(content.as_bytes())
        .and_then(|_| temp.flush())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|err| AgentError::io(format!("write temporary file for {path}"), err))?;
    if let Some(mode) = mode {
        set_mode(temp.path(), mode)?;
    }
    persist_replace(temp, target, true)?;
    Ok(json!({"bytes_written": content.len()}))
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
