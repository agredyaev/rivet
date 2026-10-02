//! Root-confined file operations exposed by the MCP tools.
//!
//! Paths are opened relative to the configured directory capabilities. Writes
//! reject final symlinks; overwrites stage a complete file before installing it.
use crate::{config::Config, mcp::Fault};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(windows)]
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(OPEN_REPARSE_POINT);
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    pub path: String,
    pub depth: Option<usize>,
    pub max_entries: Option<usize>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub path: String,
    pub offset: Option<u64>,
    pub limit: Option<usize>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    pub path: String,
    pub content: String,
    pub mode: String,
    pub expected_sha256: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRequest {
    pub path: String,
    pub old: String,
    pub new: String,
    pub expected_occurrences: usize,
    pub expected_sha256: Option<String>,
}

fn path_error(message: &str) -> Fault {
    Fault::new("PATH_DENIED", message)
}
fn io_error(message: &str, error: std::io::Error) -> Fault {
    Fault::with("IO_ERROR", message, json!({"error": error.to_string()}))
}

fn target<'a>(config: &'a Config, input: &'a str) -> Result<(&'a Dir, &'a Path), Fault> {
    let (root, relative) = config
        .root_for(Path::new(input))
        .ok_or_else(|| path_error("path is outside allowed roots or is not absolute"))?;
    Ok((&root.dir, relative))
}

fn parent(config: &Config, input: &str) -> Result<(Dir, std::ffi::OsString), Fault> {
    let (root, relative) = target(config, input)?;
    let name = relative
        .file_name()
        .ok_or_else(|| path_error("path must name a file"))?;
    if !matches!(
        relative.components().next_back(),
        Some(Component::Normal(_))
    ) {
        return Err(path_error("invalid file name"));
    }
    let parent = relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = root
        .open_dir(parent)
        .map_err(|_| path_error("parent directory is unavailable or escapes allowed roots"))?;
    Ok((dir, name.to_os_string()))
}

fn validate_sha(expected: Option<&str>) -> Result<(), Fault> {
    if let Some(value) = expected
        && (value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "expected_sha256 must be 64 hex digits",
        ));
    }
    Ok(())
}

fn current_file(parent: &Dir, name: &std::ffi::OsStr) -> Result<Option<cap_std::fs::File>, Fault> {
    match parent.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(path_error("final symlink cannot be written"))
        }
        Ok(meta) if !meta.is_file() => Err(path_error("target is not a regular file")),
        Ok(_) => parent
            .open(name)
            .map(Some)
            .map_err(|e| io_error("failed to open target", e)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("failed to inspect target", error)),
    }
}

fn check_sha(
    mut file: Option<&mut cap_std::fs::File>,
    expected: Option<&str>,
) -> Result<(), Fault> {
    validate_sha(expected)?;
    let Some(expected) = expected else {
        return Ok(());
    };
    let Some(file) = file.as_mut() else {
        return Err(Fault::new("FILE_CHANGED", "target file is missing"));
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io_error("failed to seek target", e))?;
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|e| io_error("failed to hash target", e))?;
        if count == 0 {
            break;
        }
        hash.update(&chunk[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected.to_ascii_lowercase() {
        return Err(Fault::new(
            "FILE_CHANGED",
            "target hash does not match expected_sha256",
        ));
    }
    Ok(())
}

pub fn list(config: &Config, request: ListRequest) -> Result<Value, Fault> {
    let depth = request.depth.unwrap_or(1);
    let max = request
        .max_entries
        .unwrap_or(config.limits.max_directory_entries);
    if depth == 0 || depth > 16 || max == 0 || max > config.limits.max_directory_entries {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "directory depth or entry limit is outside configured bounds",
        ));
    }
    let (root, relative) = target(config, &request.path)?;
    let dir = root
        .open_dir(if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        })
        .map_err(|_| path_error("directory is unavailable or escapes allowed roots"))?;
    let mut entries = Vec::new();
    let mut pending = vec![(dir, request.path, depth)];
    let mut truncated = false;
    while let Some((dir, prefix, left)) = pending.pop() {
        for entry in dir
            .read_dir(".")
            .map_err(|e| io_error("failed to list directory", e))?
        {
            let entry = entry.map_err(|e| io_error("failed to list directory", e))?;
            if entries.len() == max {
                truncated = true;
                break;
            }
            let kind = entry
                .file_type()
                .map_err(|e| io_error("failed to inspect entry", e))?;
            let path = Path::new(&prefix).join(entry.file_name());
            entries.push(json!({"path": path.to_string_lossy(), "type": if kind.is_dir() { "directory" } else if kind.is_symlink() { "symlink" } else { "file" }}));
            if left > 1 && kind.is_dir() {
                pending.push((
                    entry
                        .open_dir()
                        .map_err(|e| io_error("failed to open child directory", e))?,
                    path.to_string_lossy().into_owned(),
                    left - 1,
                ));
            }
        }
        if truncated {
            break;
        }
    }
    Ok(json!({"entries": entries, "truncated": truncated}))
}

pub fn read(config: &Config, request: ReadRequest) -> Result<Value, Fault> {
    let limit = request.limit.unwrap_or(config.limits.max_file_read_bytes);
    if limit == 0 || limit > config.limits.max_file_read_bytes {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "read limit is outside configured bounds",
        ));
    }
    let offset = request.offset.unwrap_or(0);
    let (root, relative) = target(config, &request.path)?;
    let mut file = root
        .open(relative)
        .map_err(|_| path_error("file is unavailable or escapes allowed roots"))?;
    let size = file
        .metadata()
        .map_err(|e| io_error("failed to inspect file", e))?
        .len();
    if offset > size {
        return Err(Fault::new(
            "INVALID_REQUEST",
            "read offset is beyond file size",
        ));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| io_error("failed to seek file", e))?;
    let mut data = Vec::with_capacity(limit.saturating_add(4));
    file.take(limit as u64 + 4)
        .read_to_end(&mut data)
        .map_err(|e| io_error("failed to read file", e))?;
    let mut end = data.len().min(limit);
    while end > 0
        && std::str::from_utf8(&data[..end])
            .is_err_and(|e| e.error_len().is_none() && e.valid_up_to() < end)
    {
        end -= 1;
    }
    let text = std::str::from_utf8(&data[..end]).map_err(|_| {
        Fault::new(
            "INVALID_REQUEST",
            "file contains invalid UTF-8 or offset splits a character",
        )
    })?;
    if end == 0 && !data.is_empty() {
        return Err(Fault::new(
            "OUTPUT_LIMIT",
            "read limit cannot hold the next UTF-8 character",
        ));
    }
    Ok(
        json!({"text": text, "next_offset": offset + end as u64, "eof": offset + end as u64 >= size}),
    )
}

fn atomic_install(
    parent: &Dir,
    name: &std::ffi::OsStr,
    content: &[u8],
    permissions: Option<cap_std::fs::Permissions>,
) -> Result<(), Fault> {
    // Stage beside the destination so rename installs complete contents at once.
    let temp = format!(
        ".rivet-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    no_follow(&mut opts);
    let mut file = parent
        .open_with(&temp, &opts)
        .map_err(|e| io_error("failed to stage file", e))?;
    let install = (|| {
        file.write_all(content)
            .map_err(|e| io_error("failed to write staged file", e))?;
        if let Some(permissions) = permissions {
            parent
                .set_permissions(&temp, permissions)
                .map_err(|e| io_error("failed to set staged permissions", e))?;
        }
        file.sync_all()
            .map_err(|e| io_error("failed to sync staged file", e))?;
        parent
            .rename(&temp, parent, name)
            .map_err(|e| io_error("failed to install staged file", e))?;
        Ok(())
    })();
    if install.is_err() {
        let _ = parent.remove_file(&temp);
    }
    install
}

pub fn write(config: &Config, request: WriteRequest) -> Result<Value, Fault> {
    if request.content.len() > config.limits.max_file_read_bytes {
        return Err(Fault::new(
            "OUTPUT_LIMIT",
            "content exceeds max_file_read_bytes",
        ));
    }
    validate_sha(request.expected_sha256.as_deref())?;
    let (parent, name) = parent(config, &request.path)?;
    let mut current = current_file(&parent, &name)?;
    check_sha(current.as_mut(), request.expected_sha256.as_deref())?;
    match request.mode.as_str() {
        "create" => {
            if current.is_some() {
                return Err(Fault::new("FILE_CHANGED", "target already exists"));
            }
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            no_follow(&mut opts);
            let mut file = parent
                .open_with(&name, &opts)
                .map_err(|e| io_error("failed to create file", e))?;
            file.write_all(request.content.as_bytes())
                .map_err(|e| io_error("failed to write file", e))?;
        }
        "overwrite" => {
            let perms = current
                .as_ref()
                .map(|file| file.metadata().map(|m| m.permissions()))
                .transpose()
                .map_err(|e| io_error("failed to inspect permissions", e))?;
            atomic_install(&parent, &name, request.content.as_bytes(), perms)?;
        }
        "append" => {
            let mut opts = OpenOptions::new();
            opts.append(true).create(true);
            no_follow(&mut opts);
            let mut file = parent
                .open_with(&name, &opts)
                .map_err(|e| io_error("failed to open append target", e))?;
            file.write_all(request.content.as_bytes())
                .map_err(|e| io_error("failed to append file", e))?;
        }
        _ => {
            return Err(Fault::new(
                "INVALID_REQUEST",
                "mode must be create, overwrite, or append",
            ));
        }
    }
    Ok(json!({"bytes_written": request.content.len()}))
}

pub fn replace(config: &Config, request: ReplaceRequest) -> Result<Value, Fault> {
    if request.old.is_empty() {
        return Err(Fault::new("INVALID_REQUEST", "old must be nonempty"));
    }
    if request.old.len() > config.limits.max_file_read_bytes
        || request.new.len() > config.limits.max_file_read_bytes
    {
        return Err(Fault::new(
            "OUTPUT_LIMIT",
            "replacement input exceeds max_file_read_bytes",
        ));
    }
    let (parent, name) = parent(config, &request.path)?;
    let mut file = current_file(&parent, &name)?
        .ok_or_else(|| Fault::new("IO_ERROR", "target file is missing"))?;
    check_sha(Some(&mut file), request.expected_sha256.as_deref())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io_error("failed to seek file", e))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(config.limits.max_file_read_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("failed to read file", e))?;
    if bytes.len() > config.limits.max_file_read_bytes {
        return Err(Fault::new(
            "OUTPUT_LIMIT",
            "file exceeds max_file_read_bytes",
        ));
    }
    let text =
        String::from_utf8(bytes).map_err(|_| Fault::new("INVALID_REQUEST", "file is not UTF-8"))?;
    let occurrences = text.matches(&request.old).count();
    if occurrences != request.expected_occurrences {
        return Err(Fault::with(
            "FILE_CHANGED",
            "replacement occurrence count differs",
            json!({"actual": occurrences, "expected": request.expected_occurrences}),
        ));
    }
    let removed = occurrences
        .checked_mul(request.old.len())
        .ok_or_else(|| Fault::new("OUTPUT_LIMIT", "replacement output length overflows"))?;
    let added = occurrences
        .checked_mul(request.new.len())
        .ok_or_else(|| Fault::new("OUTPUT_LIMIT", "replacement output length overflows"))?;
    let output_len = text
        .len()
        .checked_sub(removed)
        .and_then(|n| n.checked_add(added))
        .ok_or_else(|| Fault::new("OUTPUT_LIMIT", "replacement output length overflows"))?;
    if output_len > config.limits.max_file_read_bytes {
        return Err(Fault::new(
            "OUTPUT_LIMIT",
            "replacement output exceeds max_file_read_bytes",
        ));
    }
    let output = text.replace(&request.old, &request.new);
    let permissions = file
        .metadata()
        .map_err(|e| io_error("failed to inspect permissions", e))?
        .permissions();
    atomic_install(&parent, &name, output.as_bytes(), Some(permissions))?;
    Ok(json!({"occurrences": occurrences, "bytes_written": output.len()}))
}
