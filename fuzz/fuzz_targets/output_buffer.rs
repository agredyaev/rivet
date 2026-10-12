#![no_main]

use libfuzzer_sys::fuzz_target;
use rivet::output_buffer::{Buffer, OutputSlice, SliceError};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let cap = (data[0] as usize) + 1;
    let mut buffer = Buffer::with_capacity(cap);
    let mut captured = Vec::with_capacity(cap);
    let mut total = 0_u64;
    let mut cursor = 1_usize;

    while cursor < data.len() {
        let op = data[cursor] % 3;
        cursor += 1;

        match op {
            0 => {
                if cursor >= data.len() {
                    break;
                }
                let len = (data[cursor] as usize).min(data.len() - cursor - 1);
                cursor += 1;
                let end = cursor + len;
                let chunk = &data[cursor..end];
                cursor = end;

                buffer.append(chunk, cap);
                total = total.saturating_add(chunk.len() as u64);
                let keep = chunk.len().min(cap.saturating_sub(captured.len()));
                captured.extend_from_slice(&chunk[..keep]);

                assert_eq!(buffer.total(), total);
                assert_eq!(buffer.len(), captured.len());
                assert!(buffer.len() <= cap);
            }
            1 => {
                if cursor + 1 >= data.len() {
                    break;
                }
                let offset = (data[cursor] as u64)
                    .saturating_mul((total / 255).saturating_add(1));
                let limit = data[cursor + 1] as usize;
                cursor += 2;

                let result = buffer.slice(offset, limit, cap);
                if offset > total {
                    assert_eq!(result, Err(SliceError::OffsetBeyondTotal));
                    continue;
                }

                let start = (offset as usize).min(captured.len());
                let end = start.saturating_add(limit).min(captured.len());
                let next_offset =
                    if offset as usize >= captured.len() && total > captured.len() as u64 {
                        total
                    } else {
                        end as u64
                    };
                let expected = OutputSlice {
                    text: String::from_utf8_lossy(&captured[start..end]).into_owned(),
                    next_offset,
                    total_bytes: total,
                    truncated: total > cap as u64,
                };
                assert_eq!(result.unwrap(), expected);
            }
            _ => {
                buffer.reset();
                captured.clear();
                total = 0;
                assert_eq!(buffer.total(), 0);
                assert_eq!(buffer.len(), 0);
                assert!(buffer.capacity() >= cap);
            }
        }
    }
});
