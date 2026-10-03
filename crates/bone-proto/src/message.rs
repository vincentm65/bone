//! JSON-RPC 2.0 envelope.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const JSONRPC_VERSION: &str = "2.0";

/// Request identifier. JSON-RPC allows numbers and strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
    /// Only in error responses to requests whose id could not be read.
    Null,
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Number(n)
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s:?}"),
            RequestId::Null => f.write_str("null"),
        }
    }
}

/// JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// A request other than `initialize` arrived before the handshake.
    pub const NOT_INITIALIZED: i64 = -32002;
    /// Client and server protocol versions are incompatible.
    pub const VERSION_MISMATCH: i64 = -32003;
    /// The session already has a turn running.
    pub const BUSY: i64 = -32004;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            Self::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    pub fn invalid_params(err: impl std::fmt::Display) -> Self {
        Self::new(Self::INVALID_PARAMS, format!("invalid params: {err}"))
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        Self::new(Self::INTERNAL_ERROR, err.to_string())
    }
}

/// One JSON-RPC message, in either direction.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: RequestId,
        method: String,
        params: Option<Value>,
    },
    Response {
        id: RequestId,
        result: Result<Value, RpcError>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
}

/// Flat wire shape shared by all message kinds.
#[derive(Serialize, Deserialize)]
struct Raw {
    jsonrpc: String,
    // `"id": null` must decode as `Some(Null)`, not absent: a parse-error
    // response carries it, and reading that as malformed would bounce errors
    // between peers forever.
    #[serde(
        default,
        deserialize_with = "present_id",
        skip_serializing_if = "Option::is_none"
    )]
    id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    // `"result": null` is a valid success, so keep it distinct from absent.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

fn present_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<RequestId>, D::Error> {
    RequestId::deserialize(d).map(Some)
}

impl Serialize for Message {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut raw = Raw {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: None,
            method: None,
            params: None,
            result: None,
            error: None,
        };
        match self.clone() {
            Message::Request { id, method, params } => {
                raw.id = Some(id);
                raw.method = Some(method);
                raw.params = params;
            }
            Message::Response { id, result } => {
                raw.id = Some(id);
                match result {
                    Ok(v) => raw.result = Some(v),
                    Err(e) => raw.error = Some(e),
                }
            }
            Message::Notification { method, params } => {
                raw.method = Some(method);
                raw.params = params;
            }
        }
        raw.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = Raw::deserialize(d)?;
        if raw.jsonrpc != JSONRPC_VERSION {
            return Err(D::Error::custom(format!(
                "unsupported jsonrpc version {:?}",
                raw.jsonrpc
            )));
        }
        match (raw.id, raw.method, raw.result, raw.error) {
            (Some(id), Some(method), None, None) => Ok(Message::Request {
                id,
                method,
                params: raw.params,
            }),
            (None, Some(method), None, None) => Ok(Message::Notification {
                method,
                params: raw.params,
            }),
            (Some(id), None, Some(result), None) => Ok(Message::Response {
                id,
                result: Ok(result),
            }),
            (Some(id), None, None, Some(error)) => Ok(Message::Response {
                id,
                result: Err(error),
            }),
            _ => Err(D::Error::custom("not a valid JSON-RPC 2.0 message")),
        }
    }
}
