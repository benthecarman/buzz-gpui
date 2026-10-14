//! The Buzz tools as an MCP server inside the app (buzz-app's
//! `src/buzz-mcp/rpc.ts`): answers one JSON-RPC message at a time, as
//! Claude sends them over its stream-json pipe to an in-process
//! (`sdk`) server. Holds no state between messages.

use super::tools::{Reply, call_tool, tools};
use super::{BuzzClient, Context};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Answers `input`. Without a client, tools are listed but cannot be called.
pub async fn respond(
    client: Option<Arc<dyn BuzzClient>>,
    context: &Context,
    input: &Value,
) -> Value {
    let id = input.get("id").cloned().unwrap_or(Value::Null);
    // A notification needs no answer, but the pipe expects one.
    if id.is_null() {
        return json!({ "jsonrpc": "2.0", "id": 0, "result": {} });
    }
    let empty = Map::new();
    let params = input
        .get("params")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let result = |value: Value| json!({ "jsonrpc": "2.0", "id": id, "result": value });
    match input.get("method").and_then(Value::as_str) {
        Some("initialize") => result(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .cloned()
                .unwrap_or_else(|| json!("2025-06-18")),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "buzz", "version": "1" },
        })),
        Some("ping") => result(json!({})),
        Some("tools/list") => result(json!({ "tools": tools() })),
        Some("tools/call") => {
            let called = async {
                let client = client.ok_or("Buzz is not ready yet. Try again.")?;
                let name = match params.get("name") {
                    Some(Value::String(name)) => name.clone(),
                    Some(other) => other.to_string(),
                    None => "undefined".into(),
                };
                let args = params
                    .get("arguments")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                call_tool(client, context, &name, &args).await
            };
            match called.await {
                Ok(reply) => result(json!({ "content": content(reply) })),
                Err(text) => result(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": true,
                })),
            }
        }
        method => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("No method {}", method.unwrap_or("undefined")),
            },
        }),
    }
}

/// A tool's answer as MCP content.
pub fn content(reply: Reply) -> Value {
    match reply {
        Reply::Text(text) => json!([{ "type": "text", "text": text }]),
        Reply::Image { data, mime } => json!([{ "type": "image", "data": data, "mimeType": mime }]),
    }
}
