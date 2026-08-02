use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path};

use remote_ops_protocol::{MAX_FILE_OPERATION_ENTRIES, MAX_UNIX_MODE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::error::{AgentError, AgentResult};

use super::files::persist_replace;

pub fn mkdir(path: &str, recursive: bool, mode: Option<u32>) -> AgentResult<Value> {
    validate_path(path)?;
    validate_mode(mode)?;
    ensure_mode_supported(mode)?;
    let target = Path::new(path);
    let created = match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() {
                return Err(AgentError::invalid(
                    "path already exists and is not a directory",
                ));
            }
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if recursive {
                fs::create_dir_all(target)
            } else {
                fs::create_dir(target)
            }
            .map_err(|error| AgentError::io(format!("create directory {path}"), error))?;
            true
        }
        Err(error) => return Err(AgentError::io(format!("stat {path}"), error)),
    };
    if let Some(mode) = mode {
        set_mode(target, mode)?;
    }
    sync_parent(target)?;
    Ok(json!({"path": path, "created": created, "recursive": recursive, "mode": mode}))
}

pub fn remove(path: &str, recursive: bool) -> AgentResult<Value> {
    validate_path(path)?;
    let target = Path::new(path);
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| AgentError::io(format!("stat {path}"), error))?;
    let kind = file_kind(&metadata)?;
    let mut entries_removed = 1usize;
    if metadata.file_type().is_dir() {
        if recursive {
            reject_dangerous_recursive_target(target)?;
            entries_removed = validate_tree(target, true)?;
            fs::remove_dir_all(target)
                .map_err(|error| AgentError::io(format!("remove directory {path}"), error))?;
        } else {
            fs::remove_dir(target)
                .map_err(|error| AgentError::io(format!("remove empty directory {path}"), error))?;
        }
    } else {
        fs::remove_file(target).map_err(|error| AgentError::io(format!("remove {path}"), error))?;
    }
    sync_parent(target)?;
    Ok(
        json!({"path": path, "kind": kind, "recursive": recursive, "entries_removed": entries_removed}),
    )
}

pub fn move_path(source: &str, destination: &str, overwrite: bool) -> AgentResult<Value> {
    validate_two_paths(source, destination)?;
    let source_path = Path::new(source);
    let destination_path = Path::new(destination);
    let source_metadata = fs::symlink_metadata(source_path)
        .map_err(|error| AgentError::io(format!("stat {source}"), error))?;
    let kind = file_kind(&source_metadata)?;
    let destination_existed = fs::symlink_metadata(destination_path).is_ok();
    if let Ok(destination_metadata) = fs::symlink_metadata(destination_path) {
        if !overwrite {
            return Err(AgentError::invalid("destination already exists"));
        }
        if source_metadata.file_type().is_dir() || destination_metadata.file_type().is_dir() {
            return Err(AgentError::invalid(
                "overwrite move is limited to non-directory paths",
            ));
        }
        file_kind(&destination_metadata)?;
    }
    rename_replace(source_path, destination_path, overwrite).map_err(|error| {
        if is_cross_filesystem(&error) {
            AgentError::cross_filesystem(
                "move crosses filesystems; use copy followed by an explicit remove",
            )
        } else {
            AgentError::io(format!("move {source} to {destination}"), error)
        }
    })?;
    sync_parent(source_path)?;
    sync_parent(destination_path)?;
    Ok(json!({
        "source": source,
        "destination": destination,
        "kind": kind,
        "overwritten": destination_existed,
        "cross_filesystem": false,
        "atomic": cfg!(not(windows))
    }))
}

pub fn copy_path(
    source: &str,
    destination: &str,
    overwrite: bool,
    recursive: bool,
) -> AgentResult<Value> {
    validate_two_paths(source, destination)?;
    let source_path = Path::new(source);
    let destination_path = Path::new(destination);
    let metadata = fs::symlink_metadata(source_path)
        .map_err(|error| AgentError::io(format!("stat {source}"), error))?;
    let kind = file_kind(&metadata)?;
    let destination_existed = fs::symlink_metadata(destination_path).is_ok();
    let (entries_copied, bytes_copied) = if metadata.file_type().is_file() {
        copy_regular_file(source_path, destination_path, overwrite)?;
        (1usize, metadata.len())
    } else if metadata.file_type().is_dir() {
        if !recursive {
            return Err(AgentError::invalid(
                "recursive must be true when copying a directory",
            ));
        }
        if fs::symlink_metadata(destination_path).is_ok() {
            return Err(AgentError::invalid(
                "directory copy requires a destination that does not exist",
            ));
        }
        let count = validate_tree(source_path, false)?;
        let mut copied = 0usize;
        let mut bytes = 0u64;
        if let Err(error) = copy_tree(source_path, destination_path, &mut copied, &mut bytes) {
            let _ = fs::remove_dir_all(destination_path);
            return Err(error);
        }
        debug_assert_eq!(count, copied);
        (copied, bytes)
    } else {
        return Err(AgentError::invalid("unsupported source type"));
    };
    sync_parent(destination_path)?;
    Ok(json!({
        "source": source,
        "destination": destination,
        "kind": kind,
        "recursive": recursive,
        "overwritten": destination_existed,
        "entries_copied": entries_copied,
        "bytes_copied": bytes_copied
    }))
}

pub fn chmod(path: &str, mode: u32) -> AgentResult<Value> {
    validate_path(path)?;
    validate_mode(Some(mode))?;
    let target = Path::new(path);
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| AgentError::io(format!("stat {path}"), error))?;
    let kind = file_kind(&metadata)?;
    if metadata.file_type().is_symlink() {
        return Err(AgentError::invalid("chmod does not follow symbolic links"));
    }
    set_mode(target, mode)?;
    Ok(json!({"path": path, "kind": kind, "mode": mode}))
}

pub fn symlink(
    target: &str,
    link_path: &str,
    overwrite: bool,
    target_kind: Option<&str>,
) -> AgentResult<Value> {
    validate_two_paths(target, link_path)?;
    if target_kind.is_some_and(|kind| !matches!(kind, "file" | "dir")) {
        return Err(AgentError::invalid("target_kind must be file or dir"));
    }
    let link = Path::new(link_path);
    let destination_existed = fs::symlink_metadata(link).is_ok();
    if let Ok(metadata) = fs::symlink_metadata(link) {
        if !overwrite {
            return Err(AgentError::invalid("link_path already exists"));
        }
        if metadata.file_type().is_dir() {
            return Err(AgentError::invalid(
                "overwrite symlink refuses to replace a directory",
            ));
        }
        file_kind(&metadata)?;
    }
    let parent = link
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".remoteops-link-{}", random_suffix()?));
    create_symlink(Path::new(target), &temporary, target_kind)?;
    if let Err(error) = rename_replace(&temporary, link, overwrite) {
        let _ = fs::remove_file(&temporary);
        return Err(AgentError::io(format!("create symlink {link_path}"), error));
    }
    sync_parent(link)?;
    Ok(json!({
        "target": target,
        "link_path": link_path,
        "target_kind": target_kind,
        "overwritten": destination_existed,
        "atomic": cfg!(not(windows))
    }))
}

fn copy_regular_file(source: &Path, destination: &Path, overwrite: bool) -> AgentResult<()> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if !metadata.file_type().is_file() {
            return Err(AgentError::invalid(
                "destination must be a regular file and not a symlink",
            ));
        }
        if !overwrite {
            return Err(AgentError::invalid("destination already exists"));
        }
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut input = File::open(source)
        .map_err(|error| AgentError::io(format!("open {}", source.display()), error))?;
    let mut temp = NamedTempFile::new_in(parent).map_err(|error| {
        AgentError::io(
            format!("create temporary file for {}", destination.display()),
            error,
        )
    })?;
    std::io::copy(&mut input, &mut temp)
        .and_then(|_| temp.flush())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| AgentError::io(format!("copy {}", source.display()), error))?;
    copy_mode(source, temp.path())?;
    persist_replace(temp, destination, overwrite)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    entries: &mut usize,
    bytes: &mut u64,
) -> AgentResult<()> {
    fs::create_dir(destination).map_err(|error| {
        AgentError::io(format!("create directory {}", destination.display()), error)
    })?;
    *entries += 1;
    for item in fs::read_dir(source)
        .map_err(|error| AgentError::io(format!("read directory {}", source.display()), error))?
    {
        let item = item.map_err(|error| AgentError::io("read directory entry", error))?;
        let source_path = item.path();
        let destination_path = destination.join(item.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| AgentError::io(format!("stat {}", source_path.display()), error))?;
        if metadata.file_type().is_dir() {
            copy_tree(&source_path, &destination_path, entries, bytes)?;
        } else if metadata.file_type().is_file() {
            copy_regular_file(&source_path, &destination_path, false)?;
            *entries += 1;
            *bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| AgentError::invalid("copied byte count overflow"))?;
        } else {
            return Err(AgentError::invalid(format!(
                "directory copy encountered a symbolic link or special file: {}",
                source_path.display()
            )));
        }
    }
    copy_mode(source, destination)
}

fn validate_tree(root: &Path, allow_symlinks: bool) -> AgentResult<usize> {
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(path) = stack.pop() {
        count += 1;
        if count > MAX_FILE_OPERATION_ENTRIES {
            return Err(AgentError::invalid(format!(
                "operation exceeds {MAX_FILE_OPERATION_ENTRIES} entries"
            )));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| AgentError::io(format!("stat {}", path.display()), error))?;
        if metadata.file_type().is_dir() {
            for entry in fs::read_dir(&path).map_err(|error| {
                AgentError::io(format!("read directory {}", path.display()), error)
            })? {
                stack.push(
                    entry
                        .map_err(|error| AgentError::io("read directory entry", error))?
                        .path(),
                );
            }
        } else if metadata.file_type().is_symlink() && !allow_symlinks {
            return Err(AgentError::invalid(format!(
                "operation encountered a symbolic link: {}",
                path.display()
            )));
        } else if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            return Err(AgentError::invalid(format!(
                "operation encountered a special file: {}",
                path.display()
            )));
        }
    }
    Ok(count)
}

fn reject_dangerous_recursive_target(path: &Path) -> AgentResult<()> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AgentError::invalid(
            "recursive remove path must not contain '..' components",
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| AgentError::io(format!("canonicalize {}", path.display()), error))?;
    if canonical.parent().is_none()
        || std::env::current_dir()
            .ok()
            .and_then(|cwd| fs::canonicalize(cwd).ok())
            .is_some_and(|cwd| cwd == canonical)
    {
        return Err(AgentError::invalid(
            "recursive remove refuses filesystem roots and the Agent working directory",
        ));
    }
    Ok(())
}

fn validate_path(path: &str) -> AgentResult<()> {
    if path.is_empty() {
        Err(AgentError::invalid("path must not be empty"))
    } else if path.contains('\0') {
        Err(AgentError::invalid("path must not contain NUL"))
    } else {
        Ok(())
    }
}

fn validate_two_paths(first: &str, second: &str) -> AgentResult<()> {
    validate_path(first)?;
    validate_path(second)?;
    if Path::new(first) == Path::new(second) {
        Err(AgentError::invalid("source and destination must differ"))
    } else {
        Ok(())
    }
}

fn validate_mode(mode: Option<u32>) -> AgentResult<()> {
    if mode.is_some_and(|mode| mode > MAX_UNIX_MODE) {
        Err(AgentError::invalid(format!(
            "mode must be in range 0..={MAX_UNIX_MODE}"
        )))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_mode_supported(_mode: Option<u32>) -> AgentResult<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_mode_supported(mode: Option<u32>) -> AgentResult<()> {
    if mode.is_some() {
        Err(AgentError::unsupported(
            "Unix mode is not supported on this platform",
        ))
    } else {
        Ok(())
    }
}

fn file_kind(metadata: &fs::Metadata) -> AgentResult<&'static str> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok("file")
    } else if file_type.is_dir() {
        Ok("dir")
    } else if file_type.is_symlink() {
        Ok("symlink")
    } else {
        Err(AgentError::invalid("special files are not supported"))
    }
}

fn random_suffix() -> AgentResult<String> {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random)
        .map_err(|error| AgentError::command(format!("generate temporary name: {error}")))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash_file(path: &Path) -> AgentResult<String> {
    let mut file = File::open(path)
        .map_err(|error| AgentError::io(format!("open {}", path.display()), error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| AgentError::io(format!("read {}", path.display()), error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn verify_regular_file(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> AgentResult<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.file_type().is_file() || metadata.len() != expected_size {
        return Ok(false);
    }
    Ok(hash_file(path)?.eq_ignore_ascii_case(expected_sha256))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> AgentResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| AgentError::io(format!("chmod {}", path.display()), error))
}

pub(crate) fn set_optional_mode(path: &Path, mode: Option<u32>) -> AgentResult<()> {
    validate_mode(mode)?;
    if let Some(mode) = mode {
        set_mode(path, mode)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> AgentResult<()> {
    Err(AgentError::unsupported(
        "Unix mode is not supported on this platform",
    ))
}

#[cfg(unix)]
pub(crate) fn copy_mode(source: &Path, destination: &Path) -> AgentResult<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = fs::symlink_metadata(source)
        .map_err(|error| AgentError::io(format!("stat {}", source.display()), error))?
        .mode();
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| AgentError::io(format!("chmod {}", destination.display()), error))
}

#[cfg(not(unix))]
pub(crate) fn copy_mode(_source: &Path, _destination: &Path) -> AgentResult<()> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _kind: Option<&str>) -> AgentResult<()> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|error| AgentError::io(format!("create symlink {}", link.display()), error))
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, kind: Option<&str>) -> AgentResult<()> {
    match kind {
        Some("file") => std::os::windows::fs::symlink_file(target, link),
        Some("dir") => std::os::windows::fs::symlink_dir(target, link),
        _ => {
            return Err(AgentError::invalid(
                "target_kind is required for symlink on Windows",
            ));
        }
    }
    .map_err(|error| AgentError::io(format!("create symlink {}", link.display()), error))
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(_target: &Path, _link: &Path, _kind: Option<&str>) -> AgentResult<()> {
    Err(AgentError::unsupported(
        "symbolic links are not supported on this platform",
    ))
}

#[cfg(not(windows))]
fn rename_replace(source: &Path, destination: &Path, _overwrite: bool) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn rename_replace(source: &Path, destination: &Path, overwrite: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if overwrite {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn is_cross_filesystem(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::EXDEV)
}

#[cfg(windows)]
fn is_cross_filesystem(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(17)
}

#[cfg(not(any(unix, windows)))]
fn is_cross_filesystem(_error: &std::io::Error) -> bool {
    false
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> AgentResult<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AgentError::io(format!("sync directory {}", parent.display()), error))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> AgentResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_file_operations_keep_boundaries_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested/child");
        mkdir(&nested.to_string_lossy(), true, None).unwrap();
        let source = nested.join("source.txt");
        fs::write(&source, "payload").unwrap();
        let copy = nested.join("copy.txt");
        let result = copy_path(
            &source.to_string_lossy(),
            &copy.to_string_lossy(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(result["bytes_copied"], 7);
        let moved = nested.join("moved.txt");
        move_path(&copy.to_string_lossy(), &moved.to_string_lossy(), false).unwrap();
        assert_eq!(fs::read_to_string(&moved).unwrap(), "payload");
        remove(&moved.to_string_lossy(), false).unwrap();
        assert!(!moved.exists());
    }

    #[test]
    fn recursive_copy_rejects_symlinks_or_special_files_before_copying() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "ok").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("file", source.join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file("file", source.join("link")).unwrap();
        let destination = directory.path().join("destination");
        let error = copy_path(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            false,
            true,
        )
        .unwrap_err();
        assert!(error.message.contains("symbolic link") || error.message.contains("special"));
        assert!(!destination.exists());
    }
}
