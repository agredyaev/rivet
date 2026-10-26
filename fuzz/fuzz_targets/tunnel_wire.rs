#![no_main]

use libfuzzer_sys::fuzz_target;
use rivet::tunnel_wire::{
    extract_jsonrpc_id, parse_command, parse_poll, parse_response_timeout, valid_tunnel_id,
};
use serde_json::value::RawValue;

fuzz_target!(|data: &[u8]| {
    if let Ok(envelope) = parse_poll(data) {
        for raw in envelope.commands {
            if let Ok(command) = parse_command(raw.get().as_bytes()) {
                if let Some(timeout) = command.response_timeout.as_ref() {
                    let _ = parse_response_timeout(timeout);
                }
                if let Some(jsonrpc) = command.jsonrpc.as_deref() {
                    let _ = extract_jsonrpc_id(jsonrpc);
                }
            }
        }
    }

    if let Ok(text) = std::str::from_utf8(data) {
        let _ = valid_tunnel_id(text);
    }

    if let Ok(raw) = serde_json::from_slice::<&RawValue>(data) {
        let _ = extract_jsonrpc_id(raw);
    }
});
