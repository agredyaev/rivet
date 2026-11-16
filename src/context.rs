//! Persistent project context: flat immutable snapshots plus one append-only index.

use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const MAGIC: &[u8; 4] = b"RVCT";
const VERSION: u16 = 1;
const INDEX_ENTRY_BYTES: u64 = 16;
const FRAME_HEADER_BYTES: usize = 26;
const MAX_SEGMENT_BYTES: u64 = 1024 * 1024;
const MAX_CONTEXT_RECORDS: usize = 128;
const MAX_CONTEXT_TEXT_BYTES: usize = 32 * 1024;
const PRUNE_WATERMARK_BYTES: usize = 8;
const PRUNE_WATERMARK_FILE: &str = "context.pruned";

#[repr(u8)]
#[derive(Clone, Copy)]
enum ContextKind {
    Goal = 1,
    Responsibility = 2,
    Constraint = 3,
    Decision = 4,
    Finding = 5,
    Todo = 6,
}

impl ContextKind {
    fn from_code(code: &str) -> Option<Self> {
        match code {
            "G" => Some(Self::Goal),
            "R" => Some(Self::Responsibility),
            "C" => Some(Self::Constraint),
            "D" => Some(Self::Decision),
            "F" => Some(Self::Finding),
            "T" => Some(Self::Todo),
            _ => None,
        }
    }

    fn code(byte: u8) -> Option<u8> {
        match byte {
            1 => Some(b'G'),
            2 => Some(b'R'),
            3 => Some(b'C'),
            4 => Some(b'D'),
            5 => Some(b'F'),
            6 => Some(b'T'),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ContextError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ContextError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::new("STORAGE_ERROR", message)
    }
}

fn context_dir(workspace: &Path) -> Result<PathBuf, ContextError> {
    let dot_git = workspace.join(".git");
    let git_dir =
        if dot_git.is_dir() {
            Some(fs::canonicalize(&dot_git).map_err(|error| {
                ContextError::storage(format!("{}: {error}", dot_git.display()))
            })?)
        } else if dot_git.is_file() {
            let marker = fs::read_to_string(&dot_git).map_err(|error| {
                ContextError::storage(format!("{}: {error}", dot_git.display()))
            })?;
            let value = marker
                .trim()
                .strip_prefix("gitdir:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ContextError::storage("invalid .git file"))?;
            let path = Path::new(value);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.join(path)
            };
            Some(fs::canonicalize(&resolved).map_err(|error| {
                ContextError::storage(format!("{}: {error}", resolved.display()))
            })?)
        } else {
            None
        };

    let Some(git_dir) = git_dir else {
        return Ok(workspace.join(".rivet/context"));
    };

    let common_marker = git_dir.join("commondir");
    let common_dir = if common_marker.is_file() {
        let value = fs::read_to_string(&common_marker).map_err(|error| {
            ContextError::storage(format!("{}: {error}", common_marker.display()))
        })?;
        let value = value.trim();
        if value.is_empty() {
            return Err(ContextError::storage("empty Git commondir"));
        }
        let path = Path::new(value);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            git_dir.join(path)
        };
        fs::canonicalize(&resolved)
            .map_err(|error| ContextError::storage(format!("{}: {error}", resolved.display())))?
    } else {
        git_dir
    };

    Ok(common_dir.join("rivet/context"))
}

fn read_index_entry(file: &mut File, generation: u64) -> Result<(u64, u64), ContextError> {
    if generation == 0 {
        return Err(ContextError::new(
            "INVALID_GENERATION",
            "generation must be at least 1",
        ));
    }
    let position = generation
        .checked_sub(1)
        .and_then(|value| value.checked_mul(INDEX_ENTRY_BYTES))
        .ok_or_else(|| ContextError::new("INVALID_GENERATION", "generation is too large"))?;
    file.seek(SeekFrom::Start(position))
        .map_err(|error| ContextError::storage(format!("index seek failed: {error}")))?;
    let mut entry = [0_u8; INDEX_ENTRY_BYTES as usize];
    file.read_exact(&mut entry)
        .map_err(|error| ContextError::storage(format!("index read failed: {error}")))?;
    Ok((
        u64::from_le_bytes(entry[0..8].try_into().unwrap()),
        u64::from_le_bytes(entry[8..16].try_into().unwrap()),
    ))
}

fn read_frame_header(
    file: &mut File,
    offset: u64,
    generation: u64,
) -> Result<([u8; FRAME_HEADER_BYTES], usize, usize, usize), ContextError> {
    if offset
        .checked_add(FRAME_HEADER_BYTES as u64)
        .is_none_or(|end| end > MAX_SEGMENT_BYTES)
    {
        return Err(ContextError::storage(
            "frame header crosses segment boundary",
        ));
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(|error| ContextError::storage(format!("segment seek failed: {error}")))?;
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    file.read_exact(&mut header)
        .map_err(|error| ContextError::storage(format!("frame header read failed: {error}")))?;

    if &header[0..4] != MAGIC {
        return Err(ContextError::storage("invalid frame magic"));
    }
    if u16::from_le_bytes(header[4..6].try_into().unwrap()) != VERSION {
        return Err(ContextError::storage("invalid frame version"));
    }
    if u64::from_le_bytes(header[6..14].try_into().unwrap()) != generation {
        return Err(ContextError::storage(
            "frame generation does not match index",
        ));
    }

    let frame_bytes = u32::from_le_bytes(header[14..18].try_into().unwrap()) as usize;
    let record_count = u32::from_le_bytes(header[18..22].try_into().unwrap()) as usize;
    let text_bytes = u32::from_le_bytes(header[22..26].try_into().unwrap()) as usize;
    if record_count > MAX_CONTEXT_RECORDS {
        return Err(ContextError::storage("frame record count exceeds limit"));
    }
    if text_bytes > MAX_CONTEXT_TEXT_BYTES {
        return Err(ContextError::storage("frame text bytes exceed limit"));
    }

    let derived = FRAME_HEADER_BYTES
        .checked_add(record_count)
        .and_then(|value| value.checked_add(4_usize.checked_mul(record_count + 1)?))
        .and_then(|value| value.checked_add(text_bytes))
        .ok_or_else(|| ContextError::storage("frame size overflow"))?;
    if frame_bytes != derived {
        return Err(ContextError::storage("invalid frame size"));
    }
    if frame_bytes as u64 > MAX_SEGMENT_BYTES
        || offset
            .checked_add(frame_bytes as u64)
            .is_none_or(|end| end > MAX_SEGMENT_BYTES)
    {
        return Err(ContextError::storage("frame crosses segment boundary"));
    }

    Ok((header, frame_bytes, record_count, text_bytes))
}

pub(crate) fn encode_snapshot(snapshot: &str) -> Result<(u64, Vec<u8>), ContextError> {
    let body = snapshot.strip_suffix('\n').unwrap_or(snapshot);
    let mut lines = body.split('\n');
    let first = lines
        .next()
        .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "missing GEN line"))?;
    if first.as_bytes().contains(&b'\r') {
        return Err(ContextError::new(
            "INVALID_CONTEXT",
            "snapshot must not contain carriage returns",
        ));
    }
    let expected = first
        .strip_prefix("GEN\t")
        .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "first line must be GEN<TAB>N"))?
        .parse::<u64>()
        .map_err(|_| ContextError::new("INVALID_CONTEXT", "invalid GEN value"))?;
    let new_generation = expected
        .checked_add(1)
        .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "generation overflow"))?;

    let mut record_count = 0_usize;
    let mut text_bytes = 0_usize;
    for line in lines {
        if line.as_bytes().contains(&b'\r') {
            return Err(ContextError::new(
                "INVALID_CONTEXT",
                "snapshot must not contain carriage returns",
            ));
        }
        let (kind, text) = line
            .split_once('\t')
            .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "record must contain a tab"))?;
        ContextKind::from_code(kind)
            .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "invalid context kind"))?;
        if text.is_empty() {
            return Err(ContextError::new(
                "INVALID_CONTEXT",
                "record text must not be empty",
            ));
        }
        record_count += 1;
        if record_count > MAX_CONTEXT_RECORDS {
            return Err(ContextError::new(
                "INVALID_CONTEXT",
                "snapshot record count exceeds limit",
            ));
        }
        text_bytes = text_bytes
            .checked_add(text.len())
            .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "snapshot text size overflow"))?;
        if text_bytes > MAX_CONTEXT_TEXT_BYTES {
            return Err(ContextError::new(
                "INVALID_CONTEXT",
                "snapshot text bytes exceed limit",
            ));
        }
    }

    let frame_bytes = FRAME_HEADER_BYTES
        .checked_add(record_count)
        .and_then(|value| value.checked_add(4_usize.checked_mul(record_count + 1)?))
        .and_then(|value| value.checked_add(text_bytes))
        .ok_or_else(|| ContextError::new("INVALID_CONTEXT", "frame size overflow"))?;
    if frame_bytes as u64 > MAX_SEGMENT_BYTES {
        return Err(ContextError::new(
            "INVALID_CONTEXT",
            "snapshot does not fit in one segment",
        ));
    }

    let mut frame = vec![0_u8; frame_bytes];
    frame[0..4].copy_from_slice(MAGIC);
    frame[4..6].copy_from_slice(&VERSION.to_le_bytes());
    frame[6..14].copy_from_slice(&new_generation.to_le_bytes());
    frame[14..18].copy_from_slice(&(frame_bytes as u32).to_le_bytes());
    frame[18..22].copy_from_slice(&(record_count as u32).to_le_bytes());
    frame[22..26].copy_from_slice(&(text_bytes as u32).to_le_bytes());

    let kinds_start = FRAME_HEADER_BYTES;
    let offsets_start = kinds_start + record_count;
    let text_start = offsets_start + 4 * (record_count + 1);
    let mut text_offset = 0_usize;
    frame[offsets_start..offsets_start + 4].copy_from_slice(&0_u32.to_le_bytes());

    for (index, line) in body.split('\n').skip(1).enumerate() {
        let (kind, text) = line.split_once('\t').unwrap();
        frame[kinds_start + index] = ContextKind::from_code(kind).unwrap() as u8;
        let start = text_start + text_offset;
        frame[start..start + text.len()].copy_from_slice(text.as_bytes());
        text_offset += text.len();
        let offset = offsets_start + 4 * (index + 1);
        frame[offset..offset + 4].copy_from_slice(&(text_offset as u32).to_le_bytes());
    }

    Ok((expected, frame))
}

pub(crate) fn validate_and_format(
    frame: &[u8],
    generation: u64,
    record_count: usize,
    text_bytes: usize,
) -> Result<String, ContextError> {
    let kinds_start = FRAME_HEADER_BYTES;
    let offsets_start = kinds_start + record_count;
    let text_start = offsets_start + 4 * (record_count + 1);
    if frame.len() != text_start + text_bytes {
        return Err(ContextError::storage("frame length does not match header"));
    }

    let first_offset =
        u32::from_le_bytes(frame[offsets_start..offsets_start + 4].try_into().unwrap()) as usize;
    if first_offset != 0 {
        return Err(ContextError::storage("first text offset is not zero"));
    }

    let mut output = String::with_capacity(text_bytes + record_count * 3 + 24);
    writeln!(&mut output, "GEN\t{generation}").unwrap();

    let mut start = 0_usize;
    for index in 0..record_count {
        let code = ContextKind::code(frame[kinds_start + index])
            .ok_or_else(|| ContextError::storage("invalid context kind"))?;
        let offset = offsets_start + 4 * (index + 1);
        let end = u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap()) as usize;
        if end < start || end > text_bytes {
            return Err(ContextError::storage("invalid text offsets"));
        }
        if end == start {
            return Err(ContextError::storage("record text is empty"));
        }
        let bytes = &frame[text_start + start..text_start + end];
        if bytes.iter().any(|byte| matches!(byte, b'\n' | b'\r')) {
            return Err(ContextError::storage(
                "record text contains a line separator",
            ));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| ContextError::storage("record text is not valid UTF-8"))?;
        output.push(code as char);
        output.push('\t');
        output.push_str(text);
        output.push('\n');
        start = end;
    }
    if start != text_bytes {
        return Err(ContextError::storage(
            "final text offset does not match text bytes",
        ));
    }
    Ok(output)
}

fn pruned_before_segment(dir: &Path) -> Result<Option<u64>, ContextError> {
    let path = dir.join(PRUNE_WATERMARK_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                path.display()
            )));
        }
    };
    if bytes.len() != PRUNE_WATERMARK_BYTES {
        return Err(ContextError::storage("invalid context prune watermark"));
    }
    let segment = u64::from_le_bytes(bytes.try_into().unwrap());
    if segment == 0 {
        return Err(ContextError::storage(
            "invalid zero context prune watermark",
        ));
    }
    Ok(Some(segment))
}

fn record_pruned_before_segment(dir: &Path, segment: u64) -> Result<(), ContextError> {
    if segment == 0 {
        return Ok(());
    }
    if pruned_before_segment(dir)?.is_some_and(|current| current >= segment) {
        return Ok(());
    }

    let path = dir.join(PRUNE_WATERMARK_FILE);
    let temporary = dir.join("context.pruned.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| ContextError::storage(format!("{}: {error}", temporary.display())))?;
    file.write_all(&segment.to_le_bytes())
        .map_err(|error| ContextError::storage(format!("prune watermark write failed: {error}")))?;
    file.sync_all()
        .map_err(|error| ContextError::storage(format!("prune watermark sync failed: {error}")))?;
    fs::rename(&temporary, &path)
        .map_err(|error| ContextError::storage(format!("{}: {error}", path.display())))?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> Result<(), ContextError> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|error| ContextError::storage(format!("{}: {error}", dir.display())))
}

pub(crate) fn read(workspace: &Path, generation: Option<u64>) -> Result<String, ContextError> {
    let dir = context_dir(workspace)?;
    let index_path = dir.join("context.idx");
    let mut index = match File::open(&index_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match generation {
                None => Ok("GEN\t0\n".into()),
                Some(_) => Err(ContextError::new(
                    "INVALID_GENERATION",
                    "no persisted context exists",
                )),
            };
        }
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                index_path.display()
            )));
        }
    };

    let index_len = index
        .metadata()
        .map_err(|error| ContextError::storage(format!("index metadata failed: {error}")))?
        .len();
    let latest = index_len / INDEX_ENTRY_BYTES;
    if latest == 0 {
        return match generation {
            None => Ok("GEN\t0\n".into()),
            Some(_) => Err(ContextError::new(
                "INVALID_GENERATION",
                "no persisted context exists",
            )),
        };
    }

    let wanted = generation.unwrap_or(latest);
    if wanted == 0 || wanted > latest {
        return Err(ContextError::new(
            "INVALID_GENERATION",
            format!("generation {wanted} is outside retained index range 1..={latest}"),
        ));
    }

    let (virtual_offset, _) = read_index_entry(&mut index, wanted)?;
    let segment_id = virtual_offset / MAX_SEGMENT_BYTES;
    let offset = virtual_offset % MAX_SEGMENT_BYTES;
    let segment_path = dir.join(format!("context.{segment_id:06}.bin"));
    let mut segment = match File::open(&segment_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if pruned_before_segment(&dir)?
                .is_some_and(|first_retained| segment_id < first_retained)
            {
                return Err(ContextError::new(
                    "GENERATION_EXPIRED",
                    format!("generation {wanted} has been pruned"),
                ));
            }
            return Err(ContextError::storage(format!(
                "missing retained segment {}",
                segment_path.display()
            )));
        }
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                segment_path.display()
            )));
        }
    };

    let (header, frame_bytes, record_count, text_bytes) =
        read_frame_header(&mut segment, offset, wanted)?;
    let mut frame = vec![0_u8; frame_bytes];
    frame[..FRAME_HEADER_BYTES].copy_from_slice(&header);
    segment
        .read_exact(&mut frame[FRAME_HEADER_BYTES..])
        .map_err(|error| ContextError::storage(format!("frame read failed: {error}")))?;
    validate_and_format(&frame, wanted, record_count, text_bytes)
}

pub(crate) fn checkpoint(
    workspace: &Path,
    snapshot: &str,
    mutation: &Mutex<()>,
) -> Result<u64, ContextError> {
    let (expected, frame) = encode_snapshot(snapshot)?;
    if expected == 0 {
        let record_count = u32::from_le_bytes(frame[18..22].try_into().unwrap()) as usize;
        if record_count == 0 {
            return Err(ContextError::new(
                "INVALID_CONTEXT",
                "initial checkpoint must contain at least one record",
            ));
        }
    }
    let new_generation = expected + 1;
    let dir = context_dir(workspace)?;
    let _guard = mutation
        .lock()
        .map_err(|_| ContextError::storage("mutation lock is poisoned"))?;

    fs::create_dir_all(&dir)
        .map_err(|error| ContextError::storage(format!("{}: {error}", dir.display())))?;
    let index_path = dir.join("context.idx");
    let mut index = match OpenOptions::new().read(true).write(true).open(&index_path) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                index_path.display()
            )));
        }
    };

    let mut latest = 0_u64;
    if let Some(file) = index.as_mut() {
        let index_len = file
            .metadata()
            .map_err(|error| ContextError::storage(format!("index metadata failed: {error}")))?
            .len();
        let committed = index_len / INDEX_ENTRY_BYTES * INDEX_ENTRY_BYTES;
        if committed != index_len {
            file.set_len(committed).map_err(|error| {
                ContextError::storage(format!("index recovery failed: {error}"))
            })?;
        }
        latest = committed / INDEX_ENTRY_BYTES;
    }

    if expected != latest {
        return Err(ContextError::new(
            "CONTEXT_CHANGED",
            format!("expected generation {expected}, current generation is {latest}"),
        ));
    }

    let (current_segment, committed_end) = if latest == 0 {
        (0_u64, 0_u64)
    } else {
        let file = index
            .as_mut()
            .ok_or_else(|| ContextError::storage("index disappeared during checkpoint"))?;
        let (virtual_offset, _) = read_index_entry(file, latest)?;
        let segment_id = virtual_offset / MAX_SEGMENT_BYTES;
        let offset = virtual_offset % MAX_SEGMENT_BYTES;
        let path = dir.join(format!("context.{segment_id:06}.bin"));
        let mut segment = File::open(&path)
            .map_err(|error| ContextError::storage(format!("{}: {error}", path.display())))?;
        let (_, frame_bytes, _, _) = read_frame_header(&mut segment, offset, latest)?;
        let end = offset
            .checked_add(frame_bytes as u64)
            .ok_or_else(|| ContextError::storage("committed frame end overflow"))?;
        (segment_id, end)
    };

    let (target_segment, target_offset) = if committed_end + frame.len() as u64 <= MAX_SEGMENT_BYTES
    {
        (current_segment, committed_end)
    } else {
        (
            current_segment
                .checked_add(1)
                .ok_or_else(|| ContextError::storage("segment id overflow"))?,
            0,
        )
    };
    let segment_path = dir.join(format!("context.{target_segment:06}.bin"));
    let (mut segment, segment_created) = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&segment_path)
    {
        Ok(file) => (file, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&segment_path)
                .map_err(|error| {
                    ContextError::storage(format!("{}: {error}", segment_path.display()))
                })?,
            true,
        ),
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                segment_path.display()
            )));
        }
    };
    let segment_len = if segment_created {
        0
    } else {
        segment
            .metadata()
            .map_err(|error| ContextError::storage(format!("segment metadata failed: {error}")))?
            .len()
    };
    if segment_len < target_offset {
        return Err(ContextError::storage(
            "committed segment is shorter than the indexed frame end",
        ));
    }
    if segment_len != target_offset {
        segment
            .set_len(target_offset)
            .map_err(|error| ContextError::storage(format!("orphan recovery failed: {error}")))?;
    }
    if segment_created {
        sync_dir(&dir)?;
    }
    if target_offset != 0 {
        segment
            .seek(SeekFrom::Start(target_offset))
            .map_err(|error| ContextError::storage(format!("segment seek failed: {error}")))?;
    }
    segment
        .write_all(&frame)
        .map_err(|error| ContextError::storage(format!("frame append failed: {error}")))?;
    segment
        .sync_all()
        .map_err(|error| ContextError::storage(format!("segment sync failed: {error}")))?;

    if index.is_none() {
        index = Some(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&index_path)
                .map_err(|error| {
                    ContextError::storage(format!("{}: {error}", index_path.display()))
                })?,
        );
        sync_dir(&dir)?;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ContextError::storage("system clock is before Unix epoch"))?
        .as_secs();
    let virtual_offset = target_segment
        .checked_mul(MAX_SEGMENT_BYTES)
        .and_then(|value| value.checked_add(target_offset))
        .ok_or_else(|| ContextError::storage("virtual offset overflow"))?;
    let mut entry = [0_u8; INDEX_ENTRY_BYTES as usize];
    entry[0..8].copy_from_slice(&virtual_offset.to_le_bytes());
    entry[8..16].copy_from_slice(&timestamp.to_le_bytes());

    let file = index.as_mut().unwrap();
    file.write_all(&entry)
        .map_err(|error| ContextError::storage(format!("index append failed: {error}")))?;
    file.sync_all()
        .map_err(|error| ContextError::storage(format!("index sync failed: {error}")))?;
    Ok(new_generation)
}

#[allow(dead_code)]
pub(crate) fn prune(
    workspace: &Path,
    cutoff_unix_time_s: u64,
    min_generations_to_keep: u64,
    mutation: &Mutex<()>,
) -> Result<(), ContextError> {
    let dir = context_dir(workspace)?;
    let _guard = mutation
        .lock()
        .map_err(|_| ContextError::storage("mutation lock is poisoned"))?;
    let index_path = dir.join("context.idx");
    let mut index = match OpenOptions::new().read(true).write(true).open(&index_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ContextError::storage(format!(
                "{}: {error}",
                index_path.display()
            )));
        }
    };

    let index_len = index
        .metadata()
        .map_err(|error| ContextError::storage(format!("index metadata failed: {error}")))?
        .len();
    let committed_index_bytes = index_len / INDEX_ENTRY_BYTES * INDEX_ENTRY_BYTES;
    if committed_index_bytes != index_len {
        index
            .set_len(committed_index_bytes)
            .map_err(|error| ContextError::storage(format!("index recovery failed: {error}")))?;
    }
    let latest = committed_index_bytes / INDEX_ENTRY_BYTES;
    if latest == 0 {
        return Ok(());
    }

    let (latest_virtual_offset, _) = read_index_entry(&mut index, latest)?;
    let current_segment = latest_virtual_offset / MAX_SEGMENT_BYTES;
    let first_protected = if min_generations_to_keep == 0 {
        latest.saturating_add(1)
    } else {
        latest.saturating_sub(min_generations_to_keep - 1).max(1)
    };

    index
        .seek(SeekFrom::Start(0))
        .map_err(|error| ContextError::storage(format!("index seek failed: {error}")))?;
    let mut index = BufReader::new(index);
    let mut keep_from_segment = current_segment;
    let mut previous_segment = 0_u64;
    for generation in 1..=latest {
        let mut entry = [0_u8; INDEX_ENTRY_BYTES as usize];
        index
            .read_exact(&mut entry)
            .map_err(|error| ContextError::storage(format!("index read failed: {error}")))?;
        let virtual_offset = u64::from_le_bytes(entry[0..8].try_into().unwrap());
        let unix_time_s = u64::from_le_bytes(entry[8..16].try_into().unwrap());
        let segment_id = virtual_offset / MAX_SEGMENT_BYTES;
        if generation > 1 && segment_id < previous_segment {
            return Err(ContextError::storage(
                "index segment order is not monotonic",
            ));
        }
        if segment_id > current_segment {
            return Err(ContextError::storage("index points beyond current segment"));
        }
        previous_segment = segment_id;

        let eligible = generation < first_protected
            && unix_time_s < cutoff_unix_time_s
            && segment_id < current_segment;
        if !eligible {
            keep_from_segment = segment_id;
            break;
        }
    }

    if keep_from_segment == 0 {
        return Ok(());
    }

    record_pruned_before_segment(&dir, keep_from_segment)?;

    let mut removed = false;
    let entries = fs::read_dir(&dir)
        .map_err(|error| ContextError::storage(format!("{}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| ContextError::storage(error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(segment_id) = name
            .strip_prefix("context.")
            .and_then(|value| value.strip_suffix(".bin"))
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        if segment_id < keep_from_segment && segment_id < current_segment {
            fs::remove_file(entry.path()).map_err(|error| {
                ContextError::storage(format!("{}: {error}", entry.path().display()))
            })?;
            removed = true;
        }
    }
    if removed {
        sync_dir(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temp_workspace(label: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rivet-context-{label}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn frame(snapshot: &str) -> (u64, Vec<u8>) {
        encode_snapshot(snapshot).unwrap()
    }

    #[test]
    fn empty_snapshot_round_trip() {
        let (_, bytes) = frame("GEN\t0\n");
        let mut file_path = temp_workspace("empty");
        file_path.push("frame.bin");
        fs::write(&file_path, &bytes).unwrap();
        let mut file = File::open(&file_path).unwrap();
        let (_, size, records, text) = read_frame_header(&mut file, 0, 1).unwrap();
        assert_eq!(size, bytes.len());
        assert_eq!(
            validate_and_format(&bytes, 1, records, text).unwrap(),
            "GEN\t1\n"
        );
        let _ = fs::remove_dir_all(file_path.parent().unwrap());
    }

    #[test]
    fn non_empty_snapshot_round_trip() {
        let (_, bytes) = frame("GEN\t7\nG\tKeep it flat.\nD\tUse immutable snapshots.\n");
        let mut file_path = temp_workspace("roundtrip");
        file_path.push("frame.bin");
        fs::write(&file_path, &bytes).unwrap();
        let mut file = File::open(&file_path).unwrap();
        let (_, _, records, text) = read_frame_header(&mut file, 0, 8).unwrap();
        assert_eq!(
            validate_and_format(&bytes, 8, records, text).unwrap(),
            "GEN\t8\nG\tKeep it flat.\nD\tUse immutable snapshots.\n"
        );
        let _ = fs::remove_dir_all(file_path.parent().unwrap());
    }

    #[test]
    fn invalid_snapshot_text_is_rejected() {
        for snapshot in [
            "",
            "GEN 0",
            "GEN\tx",
            "GEN\t0\nX\tbad",
            "GEN\t0\nG\t",
            "GEN\t0\r\nG\tbad",
        ] {
            assert!(encode_snapshot(snapshot).is_err(), "{snapshot:?}");
        }
    }

    #[test]
    fn malformed_frame_fields_are_rejected() {
        let (_, bytes) = frame("GEN\t0\nG\talpha\n");
        for mutate in [
            |value: &mut Vec<u8>| value[0] = b'X',
            |value: &mut Vec<u8>| value[4] = 2,
            |value: &mut Vec<u8>| value[14] = 0,
        ] {
            let mut value = bytes.clone();
            mutate(&mut value);
            let workspace = temp_workspace("bad-frame");
            let path = workspace.join("frame.bin");
            fs::write(&path, &value).unwrap();
            let mut file = File::open(&path).unwrap();
            assert!(read_frame_header(&mut file, 0, 1).is_err());
            let _ = fs::remove_dir_all(workspace);
        }
    }

    #[test]
    fn invalid_kind_offsets_and_utf8_are_rejected() {
        let (_, bytes) = frame("GEN\t0\nG\talpha\n");
        let kinds = FRAME_HEADER_BYTES;
        let offsets = kinds + 1;
        let text = offsets + 8;

        let mut invalid_kind = bytes.clone();
        invalid_kind[kinds] = 9;
        assert!(validate_and_format(&invalid_kind, 1, 1, 5).is_err());

        let mut invalid_offsets = bytes.clone();
        invalid_offsets[offsets + 4..offsets + 8].copy_from_slice(&6_u32.to_le_bytes());
        assert!(validate_and_format(&invalid_offsets, 1, 1, 5).is_err());

        let mut invalid_utf8 = bytes;
        invalid_utf8[text] = 0xff;
        assert!(validate_and_format(&invalid_utf8, 1, 1, 5).is_err());
    }

    #[test]
    fn index_entry_is_fixed_width_and_round_trips_timestamp() {
        let workspace = temp_workspace("index");
        let path = workspace.join("index");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&123_u64.to_le_bytes()).unwrap();
        file.write_all(&456_u64.to_le_bytes()).unwrap();
        file.flush().unwrap();
        let (offset, time) = read_index_entry(&mut file, 1).unwrap();
        assert_eq!((offset, time), (123, 456));
        assert_eq!(fs::metadata(path).unwrap().len(), INDEX_ENTRY_BYTES);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn first_checkpoint_read_history_and_stale_detection() {
        let workspace = temp_workspace("checkpoint");
        let mutation = Mutex::new(());
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t0\n");
        assert_eq!(
            checkpoint(&workspace, "GEN\t0\nG\tfirst\n", &mutation).unwrap(),
            1
        );
        assert_eq!(
            checkpoint(&workspace, "GEN\t1\nG\tsecond\n", &mutation).unwrap(),
            2
        );
        assert_eq!(read(&workspace, Some(1)).unwrap(), "GEN\t1\nG\tfirst\n");
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t2\nG\tsecond\n");
        let error = checkpoint(&workspace, "GEN\t1\nG\tstale\n", &mutation).unwrap_err();
        assert_eq!(error.code, "CONTEXT_CHANGED");
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t2\nG\tsecond\n");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn reader_ignores_partial_index_tail_and_writer_truncates_it() {
        let workspace = temp_workspace("partial-index");
        let mutation = Mutex::new(());
        checkpoint(&workspace, "GEN\t0\nG\tone\n", &mutation).unwrap();
        let dir = context_dir(&workspace).unwrap();
        let index_path = dir.join("context.idx");
        OpenOptions::new()
            .append(true)
            .open(&index_path)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t1\nG\tone\n");
        checkpoint(&workspace, "GEN\t1\nG\ttwo\n", &mutation).unwrap();
        assert_eq!(
            fs::metadata(index_path).unwrap().len(),
            2 * INDEX_ENTRY_BYTES
        );
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn orphan_segment_tail_is_truncated_before_append() {
        let workspace = temp_workspace("orphan");
        let mutation = Mutex::new(());
        checkpoint(&workspace, "GEN\t0\nG\tone\n", &mutation).unwrap();
        let dir = context_dir(&workspace).unwrap();
        let segment = dir.join("context.000000.bin");
        let committed = fs::metadata(&segment).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&segment)
            .unwrap()
            .write_all(b"orphan")
            .unwrap();
        checkpoint(&workspace, "GEN\t1\nG\ttwo\n", &mutation).unwrap();
        let (_, second) = frame("GEN\t1\nG\ttwo\n");
        assert_eq!(
            fs::metadata(segment).unwrap().len(),
            committed + second.len() as u64
        );
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn rotates_when_next_frame_does_not_fit() {
        let workspace = temp_workspace("rotate");
        let mutation = Mutex::new(());
        let dir = context_dir(&workspace).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let (_, first) = frame("GEN\t0\nG\tone\n");
        let offset = MAX_SEGMENT_BYTES - first.len() as u64;
        let segment0 = dir.join("context.000000.bin");
        let mut segment = File::create(&segment0).unwrap();
        segment.set_len(offset).unwrap();
        segment.seek(SeekFrom::Start(offset)).unwrap();
        segment.write_all(&first).unwrap();
        segment.sync_all().unwrap();
        let mut index = File::create(dir.join("context.idx")).unwrap();
        index.write_all(&offset.to_le_bytes()).unwrap();
        index.write_all(&1_u64.to_le_bytes()).unwrap();
        index.sync_all().unwrap();

        checkpoint(&workspace, "GEN\t1\nG\ttwo\n", &mutation).unwrap();
        assert!(dir.join("context.000001.bin").is_file());
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t2\nG\ttwo\n");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn pruned_generation_is_distinct_from_missing_retained_segment() {
        let workspace = temp_workspace("expired");
        let mutation = Mutex::new(());
        let dir = context_dir(&workspace).unwrap();
        fs::create_dir_all(&dir).unwrap();

        let (_, first) = frame("GEN\t0\nG\tone\n");
        let (_, second) = frame("GEN\t1\nG\ttwo\n");
        fs::write(dir.join("context.000000.bin"), &first).unwrap();
        fs::write(dir.join("context.000001.bin"), &second).unwrap();
        let mut index = File::create(dir.join("context.idx")).unwrap();
        index.write_all(&0_u64.to_le_bytes()).unwrap();
        index.write_all(&1_u64.to_le_bytes()).unwrap();
        index.write_all(&MAX_SEGMENT_BYTES.to_le_bytes()).unwrap();
        index.write_all(&100_u64.to_le_bytes()).unwrap();
        index.sync_all().unwrap();

        prune(&workspace, 50, 1, &mutation).unwrap();
        let expired = read(&workspace, Some(1)).unwrap_err();
        assert_eq!(expired.code, "GENERATION_EXPIRED");

        fs::remove_file(dir.join("context.000001.bin")).unwrap();
        let missing = read(&workspace, Some(2)).unwrap_err();
        assert_eq!(missing.code, "STORAGE_ERROR");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn missing_old_segment_without_prune_is_storage_error() {
        let workspace = temp_workspace("missing-old");
        let dir = context_dir(&workspace).unwrap();
        fs::create_dir_all(&dir).unwrap();

        let (_, first) = frame("GEN\t0\nG\tone\n");
        let (_, second) = frame("GEN\t1\nG\ttwo\n");
        fs::write(dir.join("context.000000.bin"), &first).unwrap();
        fs::write(dir.join("context.000001.bin"), &second).unwrap();
        let mut index = File::create(dir.join("context.idx")).unwrap();
        index.write_all(&0_u64.to_le_bytes()).unwrap();
        index.write_all(&1_u64.to_le_bytes()).unwrap();
        index.write_all(&MAX_SEGMENT_BYTES.to_le_bytes()).unwrap();
        index.write_all(&2_u64.to_le_bytes()).unwrap();
        index.sync_all().unwrap();

        fs::remove_file(dir.join("context.000000.bin")).unwrap();
        let missing = read(&workspace, Some(1)).unwrap_err();
        assert_eq!(missing.code, "STORAGE_ERROR");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn retention_deletes_only_whole_prefix_segments() {
        let workspace = temp_workspace("prune");
        let mutation = Mutex::new(());
        let dir = context_dir(&workspace).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("context.000000.bin"), b"old").unwrap();
        fs::write(dir.join("context.000001.bin"), b"mixed").unwrap();
        fs::write(dir.join("context.000002.bin"), b"current").unwrap();

        let mut index = File::create(dir.join("context.idx")).unwrap();
        for (offset, time) in [
            (0_u64, 10_u64),
            (MAX_SEGMENT_BYTES, 10),
            (MAX_SEGMENT_BYTES + 10, 100),
            (2 * MAX_SEGMENT_BYTES, 10),
        ] {
            index.write_all(&offset.to_le_bytes()).unwrap();
            index.write_all(&time.to_le_bytes()).unwrap();
        }
        index.sync_all().unwrap();

        prune(&workspace, 50, 1, &mutation).unwrap();
        assert!(!dir.join("context.000000.bin").exists());
        assert!(dir.join("context.000001.bin").exists());
        assert!(dir.join("context.000002.bin").exists());
        assert_eq!(
            fs::metadata(dir.join("context.idx")).unwrap().len(),
            4 * INDEX_ENTRY_BYTES
        );
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn sibling_projects_use_distinct_context_directories() {
        let root = temp_workspace("sibling-projects");
        let project_a = root.join("a");
        let project_b = root.join("b");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();

        assert_ne!(
            context_dir(&project_a).unwrap(),
            context_dir(&project_b).unwrap()
        );
        assert_eq!(
            context_dir(&project_a).unwrap(),
            project_a.join(".rivet/context")
        );
        assert_eq!(
            context_dir(&project_b).unwrap(),
            project_b.join(".rivet/context")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn first_empty_checkpoint_is_rejected() {
        let workspace = temp_workspace("empty-checkpoint");
        let mutation = Mutex::new(());
        let error = checkpoint(&workspace, "GEN\t0\n", &mutation).unwrap_err();
        assert_eq!(error.code, "INVALID_CONTEXT");
        assert_eq!(read(&workspace, None).unwrap(), "GEN\t0\n");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn git_worktrees_share_common_context_directory() {
        let root = temp_workspace("worktree");
        let common = root.join("repo.git");
        let git_dir = common.join("worktrees/w1");
        let workspace = root.join("w1");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(".git"), "gitdir: ../repo.git/worktrees/w1\n").unwrap();
        fs::write(git_dir.join("commondir"), "../..\n").unwrap();

        assert_eq!(
            context_dir(&workspace).unwrap(),
            fs::canonicalize(&common).unwrap().join("rivet/context")
        );
        let _ = fs::remove_dir_all(root);
    }
}
