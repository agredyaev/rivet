//! Pure MCP JSON-RPC decoding shared by the runtime and fuzz targets.

use rmcp::model::ClientJsonRpcMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboundKind {
    Request,
    Notification,
    Unsupported,
}

pub fn decode_client_message(input: &[u8]) -> Result<ClientJsonRpcMessage, serde_json::Error> {
    serde_json::from_slice(input)
}

pub fn inbound_kind(message: &ClientJsonRpcMessage) -> InboundKind {
    match message {
        ClientJsonRpcMessage::Request(_) => InboundKind::Request,
        ClientJsonRpcMessage::Notification(_) => InboundKind::Notification,
        _ => InboundKind::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_request_notification_and_response() {
        let request =
            decode_client_message(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#)
                .unwrap();
        assert_eq!(inbound_kind(&request), InboundKind::Request);

        let notification =
            decode_client_message(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert_eq!(inbound_kind(&notification), InboundKind::Notification);

        let response = decode_client_message(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap();
        assert_eq!(inbound_kind(&response), InboundKind::Unsupported);
    }
}
