use super::*;

pub(in crate::codex::protocol) const MAX_PROTOCOL_IDENTIFIER_BYTES: usize = 16 * 1024;

/// JSON-RPC request identifiers accepted by the generated Codex schema.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InboundMessage {
    Response {
        id: RequestId,
        result: Result<Value, RpcErrorObject>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: RequestId,
        method: String,
        params: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InboundEvent {
    Notification { method: String, params: Value },
    SafetyViolation { id: RequestId, method: String },
    ConnectionClosed { category: String },
}

pub fn classify_message(value: Value) -> Result<InboundMessage, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "JSON-RPC frame must be an object".to_owned())?;

    let id = object
        .get("id")
        .cloned()
        .map(serde_json::from_value::<RequestId>)
        .transpose()
        .map_err(|_| "JSON-RPC id must be a non-negative integer or string".to_owned())?;
    let method = object
        .get("method")
        .map(|method| {
            method
                .as_str()
                .ok_or_else(|| "JSON-RPC method must be a string".to_owned())
        })
        .transpose()?;
    let has_response_fields = object.contains_key("result") || object.contains_key("error");

    match (id, method) {
        (Some(_), Some(_)) | (None, Some(_)) if has_response_fields => {
            Err("JSON-RPC request or notification cannot contain response fields".to_owned())
        }
        (Some(id), Some(method)) => Ok(InboundMessage::ServerRequest {
            id,
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(method)) => Ok(InboundMessage::Notification {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => {
            let result = match (object.get("result"), object.get("error")) {
                (Some(result), None) => Ok(result.clone()),
                (None, Some(error)) => serde_json::from_value::<RpcErrorObject>(error.clone())
                    .map(Err)
                    .map_err(|_| "invalid JSON-RPC error object".to_owned())?,
                _ => {
                    return Err(
                        "JSON-RPC response must contain exactly one of result or error".to_owned(),
                    )
                }
            };
            Ok(InboundMessage::Response { id, result })
        }
        (None, None) => Err("JSON-RPC frame has neither id nor method".to_owned()),
    }
}
