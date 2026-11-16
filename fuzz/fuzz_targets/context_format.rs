#![no_main]

#[path = "../../src/context.rs"]
mod context;

use libfuzzer_sys::fuzz_target;

const FRAME_HEADER_BYTES: usize = 26;
const MAX_CONTEXT_RECORDS: usize = 128;
const MAX_CONTEXT_TEXT_BYTES: usize = 32 * 1024;

fn body_after_gen(snapshot: &str) -> &str {
    snapshot.split_once('\n').map_or("", |(_, body)| body)
}

fuzz_target!(|data: &[u8]| {
    if let Ok(snapshot) = std::str::from_utf8(data) {
        if let Ok((expected, frame)) = context::encode_snapshot(snapshot) {
            let record_count =
                u32::from_le_bytes(frame[18..22].try_into().unwrap()) as usize;
            let text_bytes =
                u32::from_le_bytes(frame[22..26].try_into().unwrap()) as usize;
            let generation = expected.checked_add(1).unwrap();

            let rendered =
                context::validate_and_format(&frame, generation, record_count, text_bytes)
                    .expect("encoded snapshot must validate");

            if generation < u64::MAX {
                let (round_expected, round_frame) =
                    context::encode_snapshot(&rendered).expect("rendered snapshot must re-encode");
                assert_eq!(round_expected, generation);

                let round_record_count =
                    u32::from_le_bytes(round_frame[18..22].try_into().unwrap()) as usize;
                let round_text_bytes =
                    u32::from_le_bytes(round_frame[22..26].try_into().unwrap()) as usize;
                let rerendered = context::validate_and_format(
                    &round_frame,
                    generation + 1,
                    round_record_count,
                    round_text_bytes,
                )
                .expect("re-encoded snapshot must validate");

                assert_eq!(body_after_gen(&rendered), body_after_gen(&rerendered));
            }
        }
    }

    if data.len() < 3 {
        return;
    }

    let record_count = (data[0] as usize) % (MAX_CONTEXT_RECORDS + 1);
    let raw_text_bytes = u16::from_le_bytes([data[1], data[2]]) as usize;
    let text_bytes = raw_text_bytes % (MAX_CONTEXT_TEXT_BYTES + 1);

    let frame_bytes = FRAME_HEADER_BYTES + record_count + 4 * (record_count + 1) + text_bytes;
    let mut frame = vec![0_u8; frame_bytes];
    for (index, byte) in frame.iter_mut().enumerate() {
        *byte = data[(index + 3) % data.len()];
    }

    frame[18..22].copy_from_slice(&(record_count as u32).to_le_bytes());
    frame[22..26].copy_from_slice(&(text_bytes as u32).to_le_bytes());

    let generation = u64::from_le_bytes(frame[6..14].try_into().unwrap());
    let _ = context::validate_and_format(&frame, generation, record_count, text_bytes);
});
