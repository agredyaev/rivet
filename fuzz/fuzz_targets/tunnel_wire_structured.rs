#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use rivet::tunnel_wire::{
    CommandType, extract_jsonrpc_id, parse_command, parse_poll, parse_response_timeout,
    valid_tunnel_id,
};
use serde_json::{Value, json};
use std::time::Duration;

const UNITS: [&str; 6] = ["ns", "us", "ms", "s", "m", "h"];
const ALNUM: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn next<T: for<'a> Arbitrary<'a> + Default>(input: &mut Unstructured<'_>) -> T {
    T::arbitrary(input).unwrap_or_default()
}

fn expected_timeout(number: u64, unit: &str) -> Option<Duration> {
    let multiplier = match unit {
        "ns" => 1_u128,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 60 * 60 * 1_000_000_000,
        _ => return None,
    };
    let nanos = (number as u128).checked_mul(multiplier)?;
    (nanos <= i64::MAX as u128).then(|| Duration::from_nanos(nanos as u64))
}

fn tunnel_part(bytes: [u8; 32]) -> String {
    bytes
        .into_iter()
        .map(|byte| ALNUM[(byte as usize) % ALNUM.len()] as char)
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);

    let command_kind: u8 = next(&mut input);
    let command_type = match command_kind % 3 {
        0 => "jsonrpc",
        1 => "session_termination",
        _ => "future_command_type",
    };
    let request_id: String = next(&mut input);
    let shard_token: String = next(&mut input);
    let channel = next::<bool>(&mut input).then(|| next::<String>(&mut input));

    let timeout_number: u64 = next(&mut input);
    let timeout_unit = UNITS[(next::<u8>(&mut input) as usize) % UNITS.len()];
    let timeout = format!("{timeout_number}{timeout_unit}");

    let id = match next::<u8>(&mut input) % 5 {
        0 => Value::Null,
        1 => Value::Bool(next(&mut input)),
        2 => json!(next::<i64>(&mut input)),
        3 => Value::String(next(&mut input)),
        _ => json!([next::<i32>(&mut input), next::<i32>(&mut input)]),
    };

    let command = json!({
        "request_id": request_id,
        "shard_token": shard_token,
        "command_type": command_type,
        "channel": channel,
        "response_timeout": timeout,
        "jsonrpc": {
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {}
        },
        "future_field": {
            "flag": next::<bool>(&mut input),
            "number": next::<i64>(&mut input)
        }
    });
    let body = serde_json::to_vec(&json!({
        "commands": [command],
        "future_envelope_field": next::<String>(&mut input)
    }))
    .unwrap();

    let envelope = parse_poll(&body).unwrap();
    assert_eq!(envelope.commands.len(), 1);
    let parsed = parse_command(envelope.commands[0].as_ref()).unwrap();

    assert_eq!(parsed.request_id, request_id);
    assert_eq!(parsed.shard_token, shard_token);
    assert_eq!(parsed.channel, channel);
    match command_kind % 3 {
        0 => assert!(matches!(parsed.command_type, CommandType::Jsonrpc)),
        1 => assert!(matches!(parsed.command_type, CommandType::SessionTermination)),
        _ => assert!(matches!(parsed.command_type, CommandType::Other)),
    }

    assert_eq!(
        parsed
            .response_timeout
            .as_ref()
            .and_then(parse_response_timeout),
        expected_timeout(timeout_number, timeout_unit)
    );
    let expected_id = (!id.is_null()).then_some(id);
    assert_eq!(
        parsed
            .jsonrpc
            .as_deref()
            .and_then(extract_jsonrpc_id),
        expected_id
    );

    let id_part = tunnel_part(next(&mut input));
    assert!(valid_tunnel_id(&format!("tunnel_{id_part}")));
    let namespace: String = next::<[u8; 4]>(&mut input)
        .into_iter()
        .map(|byte| ALNUM[(byte as usize) % ALNUM.len()] as char)
        .collect();
    assert!(valid_tunnel_id(&format!("tunnel_{namespace}_{id_part}")));
    assert!(!valid_tunnel_id(&format!("tunnel_{id_part}-")));
});
