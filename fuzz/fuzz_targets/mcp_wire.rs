#![no_main]

use libfuzzer_sys::fuzz_target;
use rivet::mcp_wire::{decode_client_message, inbound_kind};

fuzz_target!(|data: &[u8]| {
    let Ok(message) = decode_client_message(data) else {
        return;
    };

    let kind = inbound_kind(&message);
    let encoded = serde_json::to_vec(&message).unwrap();
    let reparsed = decode_client_message(&encoded).unwrap();
    assert_eq!(inbound_kind(&reparsed), kind);

    let encoded_again = serde_json::to_vec(&reparsed).unwrap();
    let reparsed_again = decode_client_message(&encoded_again).unwrap();
    assert_eq!(inbound_kind(&reparsed_again), kind);
});
