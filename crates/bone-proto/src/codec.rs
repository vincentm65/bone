//! Newline-delimited JSON framing: one [`Message`] per line.

use crate::Message;

/// Encode a message as a single line, including the trailing `\n`.
pub fn encode(msg: &Message) -> String {
    // serde_json never emits raw newlines, so one message is always one line.
    let mut line = serde_json::to_string(msg).expect("Message serialization is infallible");
    line.push('\n');
    line
}

/// Decode one line (trailing `\r`/`\n` allowed).
pub fn decode(line: &str) -> Result<Message, serde_json::Error> {
    serde_json::from_str(line.trim_end_matches(['\r', '\n']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestId, RpcError};
    use serde_json::json;

    /// Golden cases: (wire line, decoded message). Each must decode to the
    /// message and the message must encode back to exactly the line.
    fn golden() -> Vec<(&'static str, Message)> {
        vec![
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"echo","params":{"text":"hi"}}"#,
                Message::Request {
                    id: RequestId::Number(1),
                    method: "echo".into(),
                    params: Some(json!({"text": "hi"})),
                },
            ),
            (
                r#"{"jsonrpc":"2.0","id":"a","method":"shutdown"}"#,
                Message::Request {
                    id: RequestId::String("a".into()),
                    method: "shutdown".into(),
                    params: None,
                },
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
                Message::Response {
                    id: RequestId::Number(1),
                    result: Ok(json!(null)),
                },
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"method not found: nope"}}"#,
                Message::Response {
                    id: RequestId::Number(2),
                    result: Err(RpcError::method_not_found("nope")),
                },
            ),
            (
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#,
                Message::Response {
                    id: RequestId::Null,
                    result: Err(RpcError::new(RpcError::PARSE_ERROR, "parse error")),
                },
            ),
            (
                r#"{"jsonrpc":"2.0","method":"echoed","params":{"text":"hi"}}"#,
                Message::Notification {
                    method: "echoed".into(),
                    params: Some(json!({"text": "hi"})),
                },
            ),
        ]
    }

    #[test]
    fn golden_round_trip() {
        for (line, msg) in golden() {
            assert_eq!(decode(line).unwrap(), msg, "decode {line}");
            assert_eq!(encode(&msg), format!("{line}\n"), "encode {msg:?}");
        }
    }

    #[test]
    fn rejects_malformed() {
        for line in [
            r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
            r#"{"jsonrpc":"2.0"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":"m"}}"#,
            r#"{"jsonrpc":"2.0","method":"x","result":1}"#,
            "not json",
        ] {
            assert!(decode(line).is_err(), "accepted {line}");
        }
    }
}
