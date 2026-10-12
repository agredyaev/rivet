//! Pure bounded output capture used by managed processes and fuzz targets.

#[derive(Debug, PartialEq, Eq)]
pub enum SliceError {
    OffsetBeyondTotal,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct OutputSlice {
    pub text: String,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

pub struct Buffer {
    bytes: Vec<u8>,
    total: u64,
}

impl Buffer {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(cap),
            total: 0,
        }
    }

    pub fn reset(&mut self) {
        self.bytes.clear();
        self.total = 0;
    }

    pub fn append(&mut self, chunk: &[u8], cap: usize) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        let count = chunk.len().min(cap.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&chunk[..count]);
    }

    pub fn slice(&self, offset: u64, limit: usize, cap: usize) -> Result<OutputSlice, SliceError> {
        if offset > self.total {
            return Err(SliceError::OffsetBeyondTotal);
        }

        let start = (offset as usize).min(self.bytes.len());
        let end = start.saturating_add(limit).min(self.bytes.len());
        let next_offset =
            if offset as usize >= self.bytes.len() && self.total > self.bytes.len() as u64 {
                self.total
            } else {
                end as u64
            };

        Ok(OutputSlice {
            text: String::from_utf8_lossy(&self.bytes[start..end]).into_owned(),
            next_offset,
            total_bytes: self.total,
            truncated: self.total > cap as u64,
        })
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_capture_tracks_total_and_truncation() {
        let mut buffer = Buffer::with_capacity(4);
        buffer.append(b"abcdef", 4);
        assert_eq!(buffer.len(), 4);
        assert_eq!(buffer.total(), 6);
        assert_eq!(
            buffer.slice(0, 8, 4).unwrap(),
            OutputSlice {
                text: "abcd".into(),
                next_offset: 4,
                total_bytes: 6,
                truncated: true,
            }
        );
        assert_eq!(buffer.slice(4, 8, 4).unwrap().next_offset, 6);
        assert_eq!(buffer.slice(7, 8, 4), Err(SliceError::OffsetBeyondTotal));
    }
}
