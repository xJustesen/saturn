//! The `claude` CLI's stream-json protocol: one JSON object per line, in both
//! directions.
//!
//! The chat messages (`system`, `stream_event`, `assistant`, `user`,
//! `result`) are documented. The control channel (`control_request`,
//! `control_response`, `control_cancel_request`) is only defined by the Agent
//! SDK source, so everything here was checked against recordings of the real
//! CLI: see `tests/fixtures/ai/` and `scripts/claude_wire_capture.py`.
//!
//! Parsing is tolerant on purpose. Unknown message types, unknown content
//! blocks and unknown fields are skipped, and every field the client does not
//! strictly need is optional, so a newer CLI degrades instead of breaking.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{Image, ToolContent, ToolSpec};

/// One line of the CLI's stdout.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Incoming {
    System(System),
    StreamEvent(StreamEventMsg),
    Assistant(ChatMsg),
    User(ChatMsg),
    Result(ResultMsg),
    /// The CLI asks the host something: a permission, or an MCP call.
    ControlRequest(ControlRequest),
    /// The CLI's answer to a request the host sent.
    ControlResponse(ControlResponseMsg),
    /// The CLI withdraws a `ControlRequest` it no longer needs answered.
    ControlCancelRequest {
        request_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Parse one stdout line.
pub fn parse(line: &str) -> Result<Incoming, serde_json::Error> {
    serde_json::from_str(line)
}

/// `system` messages. Only `subtype: "init"` matters to the client; it is
/// sent at the start of every turn, not when the process starts.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct System {
    #[serde(default)]
    pub subtype: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub claude_code_version: Option<String>,
    pub cwd: Option<String>,
    #[serde(rename = "apiKeySource")]
    pub api_key_source: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerStatus>,
}

impl System {
    pub fn is_init(&self) -> bool {
        self.subtype == "init"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpServerStatus {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
}

/// A partial-message event (`--include-partial-messages`).
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEventMsg {
    pub event: StreamEvent,
    /// Set when the event belongs to a subagent, not the main conversation.
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart,
    ContentBlockStart {
        #[serde(default)]
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        #[serde(default)]
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        #[serde(default)]
        index: usize,
    },
    MessageDelta,
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta {
        text: String,
    },
    /// A fragment of a tool call's JSON input.
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Other,
}

/// A content block of an `assistant` or `user` message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// A string, or a list of blocks (text, image, tool_reference).
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// The text of a `tool_result`, with any non-text parts left out.
    pub fn result_text(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    /// How many images a `tool_result` carries.
    pub fn result_images(content: &Value) -> usize {
        content.as_array().map_or(0, |blocks| {
            blocks.iter().filter(|b| b["type"] == "image").count()
        })
    }
}

/// An `assistant` or `user` line. The CLI echoes its own `user` messages:
/// tool results, interruption notes and local command output.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMsg {
    pub message: Message,
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Message {
    pub model: Option<String>,
    /// `content` is a plain string for some `user` messages; those have no
    /// blocks.
    #[serde(default, deserialize_with = "lenient_blocks")]
    pub content: Vec<ContentBlock>,
}

/// Content blocks, where a block that does not parse becomes `Other` instead
/// of failing the message, and a string content has no blocks.
fn lenient_blocks<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ContentBlock>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap_or(ContentBlock::Other))
            .collect(),
        _ => Vec::new(),
    })
}

/// The end of a turn.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResultMsg {
    /// `success` or `error_during_execution` (an interrupt lands there).
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub is_error: bool,
    pub result: Option<String>,
    /// `completed`, `aborted_streaming`, `aborted_tools`, `api_error`, …
    pub terminal_reason: Option<String>,
    pub session_id: Option<String>,
    /// List-price cost of the whole session so far, not of this turn.
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default, deserialize_with = "turn_usage")]
    pub usage: Usage,
    #[serde(default)]
    pub num_turns: u32,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default, rename = "modelUsage")]
    pub model_usage: HashMap<String, ModelUsage>,
}

/// Token counts of a turn. A turn with tool calls makes several API
/// requests; the first four fields are their sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// What the turn's last request read: how full the context is now.
    #[serde(skip)]
    pub context_tokens: u64,
}

impl Usage {
    fn input_side(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }
}

/// A result's `usage`: the turn's totals, plus `iterations` with one entry
/// for the last request, which is where the context size comes from.
fn turn_usage<'de, D: Deserializer<'de>>(d: D) -> Result<Usage, D::Error> {
    let raw = Value::deserialize(d)?;
    let mut usage: Usage = serde_json::from_value(raw.clone()).unwrap_or_default();
    let last = raw["iterations"].as_array().and_then(|i| i.last()).cloned();
    let last: Option<Usage> = last.and_then(|v| serde_json::from_value(v).ok());
    usage.context_tokens = last.unwrap_or(usage).input_side();
    Ok(usage)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelUsage {
    #[serde(default, rename = "contextWindow")]
    pub context_window: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    pub request: ControlRequestBody,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestBody {
    /// `--permission-prompt-tool stdio`: may this tool run?
    CanUseTool {
        tool_name: String,
        display_name: Option<String>,
        description: Option<String>,
        #[serde(default)]
        input: Value,
        /// Permission updates the CLI offers for an "always allow" answer.
        #[serde(default)]
        permission_suggestions: Vec<Value>,
        blocked_path: Option<String>,
        tool_use_id: Option<String>,
    },
    /// A JSON-RPC message for an in-band (`"type": "sdk"`) MCP server.
    McpMessage { server_name: String, message: Value },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponseMsg {
    pub response: ControlResponse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponse {
    /// `success` or `error`.
    #[serde(default)]
    pub subtype: String,
    pub request_id: String,
    #[serde(default)]
    pub response: Value,
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn is_success(&self) -> bool {
        self.subtype == "success"
    }
}

/// Output of `claude auth status --json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthStatus {
    #[serde(default, rename = "loggedIn")]
    pub logged_in: bool,
    #[serde(rename = "authMethod")]
    pub auth_method: Option<String>,
    #[serde(rename = "subscriptionType")]
    pub subscription_type: Option<String>,
}

// --- host → CLI -------------------------------------------------------------

/// A user turn: the text, then the images as base64 blocks.
pub fn user_message(text: &str, images: &[Image]) -> Value {
    let mut content = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() || images.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for image in images {
        content.push(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": image.media_type, "data": image.data},
        }));
    }
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
    })
}

/// A request from the host to the CLI; it answers with a `control_response`
/// carrying the same `request_id`.
pub fn control_request(request_id: &str, request: Value) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": request})
}

/// The SDK handshake. Its reply is the only sign of life before a turn.
pub fn initialize() -> Value {
    json!({"subtype": "initialize", "hooks": null})
}

pub fn interrupt() -> Value {
    json!({"subtype": "interrupt"})
}

pub fn set_model(model: &str) -> Value {
    json!({"subtype": "set_model", "model": model})
}

/// The host's answer to a `ControlRequest` from the CLI.
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
}

pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": error},
    })
}

/// Allow a tool call. `input` is what the tool runs with; `remember` are
/// entries of the request's `permission_suggestions` to apply, which is how
/// "always allow" works.
pub fn permission_allow(input: Value, remember: &[Value]) -> Value {
    let mut out = json!({"behavior": "allow", "updatedInput": input});
    if !remember.is_empty() {
        out["updatedPermissions"] = Value::Array(remember.to_vec());
    }
    out
}

/// Refuse a tool call; the model is shown `message`.
pub fn permission_deny(message: &str) -> Value {
    json!({"behavior": "deny", "message": message})
}

/// Wrap a JSON-RPC reply for an in-band MCP server.
pub fn mcp_response(rpc: Value) -> Value {
    json!({"mcp_response": rpc})
}

pub fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// What a notification gets: there is no JSON-RPC reply, but the control
/// request that carried it still wants an answer.
pub fn rpc_ack() -> Value {
    json!({"jsonrpc": "2.0", "result": {}})
}

/// The MCP `initialize` result, echoing the protocol version the CLI asked for.
pub fn mcp_initialize_result(server: &str, params: &Value) -> Value {
    json!({
        "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
        "capabilities": {"tools": {}},
        "serverInfo": {"name": server, "version": env!("CARGO_PKG_VERSION")},
    })
}

pub fn mcp_tools_list(tools: &[ToolSpec]) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema}))
        .collect();
    json!({"tools": tools})
}

pub fn mcp_tool_result(content: &[ToolContent], is_error: bool) -> Value {
    let content: Vec<Value> = content
        .iter()
        .map(|c| match c {
            ToolContent::Text(text) => json!({"type": "text", "text": text}),
            ToolContent::Image(image) => {
                json!({"type": "image", "data": image.data, "mimeType": image.media_type})
            }
        })
        .collect();
    json!({"content": content, "isError": is_error})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_types_and_fields_are_tolerated() {
        assert!(matches!(
            parse(r#"{"type":"rate_limit_event","x":1}"#).unwrap(),
            Incoming::Unknown
        ));
        let Incoming::System(s) =
            parse(r#"{"type":"system","subtype":"status","new_field":[1]}"#).unwrap()
        else {
            panic!("not a system message");
        };
        assert!(!s.is_init());
        let Incoming::StreamEvent(e) =
            parse(r#"{"type":"stream_event","event":{"type":"brand_new","x":1},"parent_tool_use_id":null}"#).unwrap()
        else {
            panic!("not a stream event");
        };
        assert!(matches!(e.event, StreamEvent::Other));
        let Incoming::ControlRequest(r) = parse(
            r#"{"type":"control_request","request_id":"a","request":{"subtype":"future_thing"}}"#,
        )
        .unwrap() else {
            panic!("not a control request");
        };
        assert!(matches!(r.request, ControlRequestBody::Other));
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"no_type":1}"#).is_err());
    }

    #[test]
    fn odd_content_does_not_fail_the_message() {
        // String content (local command output) and a block without a type.
        let Incoming::User(m) =
            parse(r#"{"type":"user","message":{"role":"user","content":"<stdout>x</stdout>"}}"#)
                .unwrap()
        else {
            panic!("not a user message");
        };
        assert!(m.message.content.is_empty());
        let Incoming::Assistant(m) = parse(
            r#"{"type":"assistant","message":{"content":[{"oops":1},{"type":"hologram"},{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap() else {
            panic!("not an assistant message");
        };
        assert!(matches!(
            m.message.content[..],
            [
                ContentBlock::Other,
                ContentBlock::Other,
                ContentBlock::Text { .. }
            ]
        ));
    }

    #[test]
    fn tool_result_text_and_images() {
        let blocks = json!([
            {"type": "text", "text": "a"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AA=="}},
            {"type": "text", "text": "b"},
        ]);
        assert_eq!(ContentBlock::result_text(&blocks), "a\nb");
        assert_eq!(ContentBlock::result_images(&blocks), 1);
        assert_eq!(ContentBlock::result_text(&json!("plain")), "plain");
        assert_eq!(ContentBlock::result_images(&json!("plain")), 0);
    }

    #[test]
    fn usage_totals_and_context() {
        let line = json!({"type": "result", "subtype": "success", "usage": {
            "input_tokens": 6, "output_tokens": 196,
            "cache_read_input_tokens": 65905, "cache_creation_input_tokens": 9398,
            "iterations": [{"input_tokens": 2, "output_tokens": 55,
                "cache_read_input_tokens": 25136, "cache_creation_input_tokens": 152}],
        }});
        let Incoming::Result(res) = parse(&line.to_string()).unwrap() else {
            panic!("not a result")
        };
        assert_eq!((res.usage.input_tokens, res.usage.output_tokens), (6, 196));
        assert_eq!(res.usage.context_tokens, 2 + 25136 + 152);
        // Without `iterations` the totals are the best guess; no usage is zero.
        let Incoming::Result(res) =
            parse(r#"{"type":"result","usage":{"input_tokens":5,"cache_read_input_tokens":10}}"#)
                .unwrap()
        else {
            panic!("not a result");
        };
        assert_eq!(res.usage.context_tokens, 15);
        let Incoming::Result(res) = parse(r#"{"type":"result"}"#).unwrap() else {
            panic!("not a result")
        };
        assert_eq!(res.usage, Usage::default());
    }

    #[test]
    fn user_message_shape() {
        let msg = user_message("look", &[Image::png(&[1, 2, 3])]);
        assert_eq!(msg["type"], "user");
        assert_eq!(msg["message"]["role"], "user");
        assert_eq!(
            msg["message"]["content"][0],
            json!({"type": "text", "text": "look"})
        );
        assert_eq!(
            msg["message"]["content"][1],
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AQID"}})
        );
        // An image alone needs no empty text block; empty text alone keeps one.
        assert_eq!(
            user_message("", &[Image::png(&[1])])["message"]["content"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            user_message("", &[])["message"]["content"][0]["type"],
            "text"
        );
    }

    #[test]
    fn control_builders() {
        assert_eq!(
            control_request("saturn_1", interrupt()),
            json!({"type": "control_request", "request_id": "saturn_1", "request": {"subtype": "interrupt"}})
        );
        assert_eq!(
            control_success("r", permission_deny("no")),
            json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": "r", "response": {"behavior": "deny", "message": "no"}}})
        );
        let allow = permission_allow(json!({"command": "ls"}), &[]);
        assert_eq!(
            allow,
            json!({"behavior": "allow", "updatedInput": {"command": "ls"}})
        );
        let always = permission_allow(json!({}), &[json!({"type": "addRules"})]);
        assert_eq!(always["updatedPermissions"], json!([{"type": "addRules"}]));
        assert_eq!(control_error("r", "boom")["response"]["subtype"], "error");
    }

    #[test]
    fn mcp_builders() {
        let tools = [ToolSpec {
            name: "dummy".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }];
        assert_eq!(
            mcp_tools_list(&tools),
            json!({"tools": [{"name": "dummy", "description": "d", "inputSchema": {"type": "object"}}]})
        );
        let result = mcp_tool_result(
            &[
                ToolContent::Text("t".into()),
                ToolContent::Image(Image::png(&[1, 2, 3])),
            ],
            false,
        );
        assert_eq!(
            result,
            json!({"content": [
                {"type": "text", "text": "t"},
                {"type": "image", "data": "AQID", "mimeType": "image/png"},
            ], "isError": false})
        );
        let init = mcp_initialize_result("saturn", &json!({"protocolVersion": "2025-11-25"}));
        assert_eq!(init["protocolVersion"], "2025-11-25");
        assert_eq!(init["serverInfo"]["name"], "saturn");
        assert_eq!(rpc_result(&json!(3), json!({}))["id"], 3);
        assert_eq!(rpc_error(&json!(3), -32601, "x")["error"]["code"], -32601);
    }
}
