//! Pure OpenAI tunnel wire decoding helpers.
//!
//! This module owns no I/O and retains no state. Keep it suitable for fuzzing.

use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::time::Duration;

#[derive(Deserialize)]
pub struct PollEnvelope {
    #[serde(default)]
    pub commands: Vec<Box<RawValue>>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandType {
    Jsonrpc,
    SessionTermination,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub struct WireCommand {
    pub request_id: String,
    pub shard_token: String,
    pub command_type: CommandType,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub response_timeout: Option<Value>,
    #[serde(default)]
    pub jsonrpc: Option<Box<RawValue>>,
}

pub fn parse_poll(input: &[u8]) -> Result<PollEnvelope, serde_json::Error> {
    serde_json::from_slice(input)
}

pub fn parse_command(input: &RawValue) -> Result<WireCommand, serde_json::Error> {
    serde_json::from_str(input.get())
}

pub fn parse_response_timeout(value: &Value) -> Option<Duration> {
    let value = value.as_str()?;
    if value.is_empty() || !value.is_ascii() {
        return None;
    }
    let split = value.bytes().position(|byte| !byte.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (number, unit) = value.split_at(split);
    if number.bytes().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    let number: u128 = number.parse().ok()?;
    let multiplier: u128 = match unit {
        "ns" => 1,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 60 * 60 * 1_000_000_000,
        _ => return None,
    };
    let nanos = number.checked_mul(multiplier)?;
    if nanos > i64::MAX as u128 {
        return None;
    }
    Some(Duration::from_nanos(nanos as u64))
}

pub fn extract_jsonrpc_id(raw: &RawValue) -> Option<Value> {
    #[derive(Deserialize)]
    struct IdOnly {
        #[serde(default)]
        id: Option<Value>,
    }
    serde_json::from_str::<IdOnly>(raw.get()).ok()?.id
}

pub fn valid_tunnel_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("tunnel_") else {
        return false;
    };
    let valid32 = |part: &str| {
        part.len() == 32
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    if valid32(rest) {
        return true;
    }
    let Some((namespace, id)) = rest.split_once('_') else {
        return false;
    };
    namespace.len() == 4
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && valid32(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_fails_open() {
        assert_eq!(
            parse_response_timeout(&Value::String("30s".into())),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_response_timeout(&Value::String("0s".into())),
            Some(Duration::ZERO)
        );
        for value in [
            Value::String("4.5s".into()),
            Value::String("-1s".into()),
            Value::String("1m30s".into()),
            Value::String("30d".into()),
            Value::Number(30.into()),
            Value::Null,
        ] {
            assert_eq!(parse_response_timeout(&value), None);
        }
    }

    #[test]
    fn tunnel_id_shape_is_strict() {
        assert!(valid_tunnel_id("tunnel_0123456789abcdefghijklmnopqrstuv"));
        assert!(valid_tunnel_id(
            "tunnel_ab12_0123456789abcdefghijklmnopqrstuv"
        ));
        for invalid in [
            "",
            "tunnel_",
            "tunnel_0123",
            "tunnel_0123456789ABCDEFGHIJKLMNOPQRSTUV",
            "tunnel_ab1_0123456789abcdefghijklmnopqrstuv",
            "tunnel_ab12_0123456789abcdefghijklmnopqrstu-",
            "other_0123456789abcdefghijklmnopqrstuv",
        ] {
            assert!(!valid_tunnel_id(invalid), "{invalid}");
        }
    }

    #[test]
    fn poll_accepts_future_fields_and_preserves_raw_jsonrpc() {
        let body = br#"{"commands":[{
          "request_id":"req_1","shard_token":"opaque","command_type":"jsonrpc","channel":"main",
          "headers":{"Mcp-Protocol-Version":["2026-07-28","future"]},
          "response_timeout":"30s",
          "jsonrpc":{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}},
          "future_field":{"ignored":true}
        }]}"#;
        let envelope = parse_poll(body).unwrap();
        let command = parse_command(envelope.commands[0].as_ref()).unwrap();
        assert!(command.jsonrpc.unwrap().get().contains("tools/list"));
    }
}
