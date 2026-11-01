//! The Anthropic Messages API, for `kind = "anthropic"`: the request shape,
//! the streamed events, the catalog, and a price table.
//!
//! What differs from Chat Completions, and what Reeve does about it:
//! - `max_tokens` is required. The catalog says each model's ceiling; until
//!   it has been read, [`DEFAULT_MAX_TOKENS`] is sent.
//! - Tool results are `tool_result` blocks in a `user` turn, and one round's
//!   results go in one turn.
//! - Thinking blocks are not sent back. The current models bind a block to
//!   the exact system prompt and tools it was made under, and Reeve rebuilds
//!   its system prompt every round (reeved's brief, memory, skills), so a
//!   replayed block would be refused. A history without them is accepted;
//!   the model reasons afresh each round.
//! - The catalog carries no prices. [`rates`] has Anthropic's published list
//!   prices; `[pricing]` in config.toml overrides them.
//! - `usage.input_tokens` excludes cache reads and writes, which Reeve's
//!   `Usage.input_tokens` includes.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::llm::{CompletionRequest, ModelInfo, StreamDelta, ToolSpec};
use crate::spend::{Usage, undated};

/// The API version every request names.
pub const API_VERSION: &str = "2026-10-21";
/// Output ceiling when the catalog hasn't said the model's. Thinking counts
/// against it, so it is generous; the current models allow 128k.
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

/// What the catalog said about a model, used in the requests that follow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelFacts {
    /// Largest `max_tokens` the model takes.
    pub max_output: Option<u32>,
    /// Whether it accepts `thinking: {type: adaptive}`.
    pub adaptive: Option<bool>,
    /// Whether it accepts `output_config.effort`.
    pub effort: Option<bool>,
}

/// The request body.
pub fn body(req: &CompletionRequest, facts: Option<&ModelFacts>) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role.as_str() {
            "assistant" => {
                let mut blocks = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type": "text", "text": m.content}));
                }
                for c in m.tool_calls.iter().flatten() {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": input_object(&c.arguments),
                    }));
                }
                push_turn(&mut messages, "assistant", blocks);
            }
            "tool" => {
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                });
                if !m.content.is_empty() {
                    block["content"] = json!(m.content);
                }
                push_turn(&mut messages, "user", vec![block]);
            }
            // The owner, or a note Reeve adds to the conversation.
            _ => {
                if !m.content.is_empty() {
                    push_turn(
                        &mut messages,
                        "user",
                        vec![json!({"type": "text", "text": m.content})],
                    );
                }
            }
        }
    }
    // A rolling breakpoint: an agent loop resends the whole conversation
    // every round, and everything up to the previous round reads from cache.
    if let Some(last) = messages
        .last_mut()
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|a| a.last_mut())
    {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if let Some(cap) = facts.and_then(|f| f.max_output) {
        max_tokens = max_tokens.min(cap);
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if let Some(sys) = req.system.as_deref().filter(|s| !s.is_empty()) {
        body["system"] = json!([{
            "type": "text",
            "text": sys,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    if !req.tools.is_empty() {
        body["tools"] = tools_anthropic(&req.tools);
    }
    // `reasoning = "default"` sends nothing, and the model does what it does
    // on its own. Models before the 4.6 generation take neither setting.
    if let Some(effort) = &req.reasoning {
        let adaptive = facts
            .and_then(|f| f.adaptive)
            .unwrap_or_else(|| adaptive_by_id(&req.model));
        if adaptive {
            body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
        }
        if facts.and_then(|f| f.effort).unwrap_or(adaptive) {
            body["output_config"] = json!({"effort": effort});
        }
    }
    body
}

/// Add blocks to the conversation, joining a turn of the same role: one
/// round's tool results go in one `user` turn.
fn push_turn(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match messages.last_mut() {
        Some(last) if last["role"] == role => {
            if let Some(a) = last["content"].as_array_mut() {
                a.extend(blocks);
            }
        }
        _ => messages.push(json!({"role": role, "content": blocks})),
    }
}

/// A call's arguments as the object the API wants. No arguments is `{}`,
/// and so is anything that isn't JSON: the model was told it was wrong at
/// the time, and that answer is in the transcript.
fn input_object(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

fn tools_anthropic(tools: &[ToolSpec]) -> Value {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

/// Whether a model takes adaptive thinking, from its id, for when the
/// catalog hasn't said: the 4.6 generation and later. Ids are
/// `claude-<line>-<major>[-<minor>][-<yyyymmdd>]`, or `claude-3-5-haiku-…`
/// for the oldest.
pub fn adaptive_by_id(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("claude-") else {
        return false;
    };
    let number = |p: Option<&str>| {
        p.filter(|p| p.len() < 8)
            .and_then(|p| p.parse::<u32>().ok())
    };
    let mut nums = rest.split('-').skip_while(|p| p.parse::<u32>().is_err());
    let Some(major) = number(nums.next()) else {
        return false;
    };
    let minor = number(nums.next()).unwrap_or(0);
    major > 4 || (major == 4 && minor >= 6)
}

// ── the stream ──────────────────────────────────────────────────────────────

/// Where the stream stands after an event.
#[derive(Debug)]
pub enum End {
    /// More to come.
    Open,
    /// `message_stop`: the answer is whole.
    Done,
    /// The provider said so, or sent something unreadable.
    Failed(Error),
}

/// Parser state across events: the content block that is open.
#[derive(Debug, Default)]
pub struct EventState {
    block: Option<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Text,
    Thinking,
    ToolUse { got_input: bool },
    Other,
}

/// One `data:` payload into deltas.
pub fn parse_event(st: &mut EventState, data: &str) -> (Vec<StreamDelta>, End) {
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            return (
                Vec::new(),
                End::Failed(Error::Provider(format!("messages sse: {e}: {data}"))),
            );
        }
    };
    let mut out = Vec::new();
    let text = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match v["type"].as_str().unwrap_or("") {
        "message_start" => {
            if let Some(u) = usage_from(&v["message"]["usage"]) {
                out.push(StreamDelta::Usage(u));
            }
        }
        "content_block_start" => {
            let cb = &v["content_block"];
            st.block = Some(match cb["type"].as_str().unwrap_or("") {
                "text" => {
                    if let Some(t) = text(cb, "text") {
                        out.push(StreamDelta::Text(t));
                    }
                    Block::Text
                }
                "thinking" => {
                    if let Some(t) = text(cb, "thinking") {
                        out.push(StreamDelta::Reasoning(t));
                    }
                    Block::Thinking
                }
                "tool_use" => {
                    out.push(StreamDelta::ToolCall {
                        id: cb["id"].as_str().unwrap_or("").to_string(),
                        name: cb["name"].as_str().unwrap_or("").to_string(),
                        arguments: String::new(),
                    });
                    Block::ToolUse { got_input: false }
                }
                _ => Block::Other,
            });
        }
        "content_block_delta" => {
            let d = &v["delta"];
            match d["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    if let Some(t) = text(d, "text") {
                        out.push(StreamDelta::Text(t));
                    }
                }
                "thinking_delta" => {
                    if let Some(t) = text(d, "thinking") {
                        out.push(StreamDelta::Reasoning(t));
                    }
                }
                "input_json_delta" => {
                    if let Some(j) = text(d, "partial_json") {
                        if let Some(Block::ToolUse { got_input }) = &mut st.block {
                            *got_input = true;
                        }
                        out.push(StreamDelta::ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: j,
                        });
                    }
                }
                // Signatures and citations: nothing Reeve keeps.
                _ => {}
            }
        }
        "content_block_stop" => {
            // A call without arguments streams none; its arguments are `{}`.
            if st.block == Some(Block::ToolUse { got_input: false }) {
                out.push(StreamDelta::ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: "{}".into(),
                });
            }
            st.block = None;
        }
        "message_delta" => {
            if let Some(u) = usage_from(&v["usage"]) {
                out.push(StreamDelta::Usage(u));
            }
            match v["delta"]["stop_reason"].as_str() {
                // The output limit, or the context window filling up mid-reply:
                // either way the answer is cut short.
                Some("max_tokens" | "model_context_window_exceeded") => {
                    out.push(StreamDelta::Truncated);
                }
                Some("refusal") => {
                    let details = if v["delta"]["stop_details"].is_object() {
                        &v["delta"]["stop_details"]
                    } else {
                        &v["stop_details"]
                    };
                    return (out, End::Failed(refusal(details)));
                }
                _ => {}
            }
        }
        "message_stop" => return (out, End::Done),
        "error" => {
            return (
                out,
                End::Failed(Error::Provider(format!(
                    "the provider failed mid-reply: {}",
                    super::parse::error_text(&v["error"])
                ))),
            );
        }
        // `ping`, and anything newer.
        _ => {}
    }
    (out, End::Open)
}

/// Reeve counts cache reads and writes inside `input_tokens`; the API
/// reports them beside it.
fn usage_from(v: &Value) -> Option<Usage> {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (fresh, read, write, out) = (
        n("input_tokens"),
        n("cache_read_input_tokens"),
        n("cache_creation_input_tokens"),
        n("output_tokens"),
    );
    if fresh + read + write + out == 0 {
        return None;
    }
    Some(Usage {
        input_tokens: fresh + read + write,
        output_tokens: out,
        cached_tokens: read,
        cache_write_tokens: write,
    })
}

/// The model's safety classifier stopped the reply (`stop_reason: refusal`).
fn refusal(details: &Value) -> Error {
    let category = details["category"]
        .as_str()
        .map(|c| format!(" ({c})"))
        .unwrap_or_default();
    let why = details["explanation"]
        .as_str()
        .map(|e| format!(": {e}"))
        .unwrap_or_default();
    Error::Provider(format!(
        "Anthropic's safety classifier declined this reply{category}{why}. Rephrase, or try another model."
    ))
}

// ── the catalog ─────────────────────────────────────────────────────────────

/// One page of `GET /models`.
#[derive(Debug, Default)]
pub struct Catalog {
    /// The models, with list prices attached.
    pub models: Vec<ModelInfo>,
    /// What each model takes, by id (and by the id without its date).
    pub facts: HashMap<String, ModelFacts>,
    /// `after_id` for the next page, when there is one.
    pub next: Option<String>,
}

/// Read one page. Retired models are left out: a request naming one fails.
pub fn parse_models_json(text: &str) -> Result<Catalog> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| Error::Provider(format!("models json: {e}")))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Provider("models: missing data[]".into()))?;
    let mut cat = Catalog::default();
    for m in data {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        if m.get("lifecycle").and_then(Value::as_str) == Some("retired") {
            continue;
        }
        let caps = &m["capabilities"];
        let supported = |path: &[&str]| {
            let mut cur = caps;
            for p in path {
                cur = cur.get(p)?;
            }
            cur.get("supported").and_then(Value::as_bool)
        };
        let facts = ModelFacts {
            max_output: m
                .get("max_tokens")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0),
            adaptive: supported(&["thinking", "types", "adaptive"]),
            effort: supported(&["effort"]),
        };
        let price = rates(id);
        cat.models.push(ModelInfo {
            id: id.to_string(),
            name: m
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            context_length: m
                .get("max_input_tokens")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0),
            input_per_million: price.map(|p| p.input),
            output_per_million: price.map(|p| p.output),
            cache_read_per_million: price.map(|p| p.cache_read),
            cache_write_per_million: price.map(|p| p.cache_write),
            // Every model in this catalog is a chat model that calls tools.
            tools: Some(true),
            chat: Some(true),
        });
        if let Some(short) = undated(id) {
            cat.facts
                .entry(short.to_string())
                .or_insert_with(|| facts.clone());
        }
        cat.facts.insert(id.to_string(), facts);
    }
    if v["has_more"].as_bool() == Some(true) {
        cat.next = v["last_id"].as_str().map(str::to_string);
    }
    Ok(cat)
}

/// Anthropic's list price for a model, USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListPrice {
    /// Fresh input.
    pub input: f64,
    /// Cache writes, at the 5-minute lifetime Reeve asks for.
    pub cache_write: f64,
    /// Cache reads.
    pub cache_read: f64,
    /// Output.
    pub output: f64,
}

/// List prices as published at platform.claude.com/docs/en/about-claude/pricing
/// on 2026-11-01: id, input, 5-minute cache write, cache read, output.
const PRICES: &[(&str, f64, f64, f64, f64)] = &[
    ("claude-fable-5-1", 10.0, 12.5, 0.25, 50.0),
    ("claude-mythos-5-1", 10.0, 12.5, 0.25, 50.0),
    ("claude-fable-5", 10.0, 12.5, 1.0, 50.0),
    ("claude-mythos-5", 10.0, 12.5, 1.0, 50.0),
    ("claude-opus-5-5", 4.0, 5.0, 0.2, 20.0),
    ("claude-opus-5", 5.0, 6.25, 0.5, 25.0),
    ("claude-opus-4-8", 5.0, 6.25, 0.5, 25.0),
    ("claude-opus-4-7", 5.0, 6.25, 0.5, 25.0),
    ("claude-opus-4-6", 5.0, 6.25, 0.5, 25.0),
    ("claude-opus-4-5", 5.0, 6.25, 0.5, 25.0),
    ("claude-opus-4-1", 15.0, 18.75, 1.5, 75.0),
    ("claude-opus-4", 15.0, 18.75, 1.5, 75.0),
    ("claude-sonnet-5-5", 2.0, 2.5, 0.1, 10.0),
    ("claude-sonnet-5", 2.0, 2.5, 0.2, 10.0),
    ("claude-sonnet-4-6", 3.0, 3.75, 0.3, 15.0),
    ("claude-sonnet-4-5", 3.0, 3.75, 0.3, 15.0),
    ("claude-sonnet-4", 3.0, 3.75, 0.3, 15.0),
    // Prompts up to 100k tokens. Over that, Anthropic charges five times
    // these rates, which the price book can't say; the ledger undercounts
    // such a call.
    ("claude-haiku-5-5", 0.10, 0.125, 0.01, 0.50),
    ("claude-haiku-4-5", 1.0, 1.25, 0.1, 5.0),
    ("claude-3-5-haiku", 0.8, 1.0, 0.08, 4.0),
];

/// The list price of a model, by its id with any date dropped.
pub fn rates(id: &str) -> Option<ListPrice> {
    let lower = id.to_ascii_lowercase();
    let key = undated(&lower).unwrap_or(&lower);
    PRICES
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|&(_, input, cache_write, cache_read, output)| ListPrice {
            input,
            cache_write,
            cache_read,
            output,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{AssistantToolCall, Message, ToolCallAccumulator};

    fn req(model: &str, reasoning: Option<&str>) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            system: Some("You are Reeve.".into()),
            messages: vec![
                Message::new("user", "what's on the disk?"),
                Message {
                    tool_calls: Some(vec![
                        AssistantToolCall {
                            id: "toolu_01".into(),
                            name: "fs_read".into(),
                            arguments: "{\"path\":\"/etc/fstab\"}".into(),
                        },
                        AssistantToolCall {
                            id: "toolu_02".into(),
                            name: "sys_info".into(),
                            arguments: String::new(),
                        },
                    ]),
                    ..Message::new("assistant", "Let me look.")
                },
                Message {
                    tool_call_id: Some("toolu_01".into()),
                    ..Message::new("tool", "/dev/sda1 / ext4")
                },
                Message {
                    tool_call_id: Some("toolu_02".into()),
                    ..Message::new("tool", "")
                },
            ],
            tools: vec![ToolSpec {
                name: "fs_read".into(),
                description: "Read a file.".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            }],
            max_tokens: None,
            reasoning: reasoning.map(str::to_string),
            route: None,
        }
    }

    #[test]
    fn the_body_is_the_messages_shape() {
        let b = body(&req("claude-opus-5-5", Some("medium")), None);
        assert_eq!(b["stream"], true);
        assert_eq!(b["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(b["system"][0]["text"], "You are Reeve.");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["name"], "fs_read");
        assert!(b["tools"][0]["input_schema"].is_object());
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "medium");
        let msgs = b["messages"].as_array().unwrap();
        // user, assistant, and both tool results in one user turn.
        assert_eq!(msgs.len(), 3, "{msgs:#?}");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["text"], "what's on the disk?");
        let reply = msgs[1]["content"].as_array().unwrap();
        assert_eq!(reply[0]["type"], "text");
        assert_eq!(reply[1]["type"], "tool_use");
        assert_eq!(reply[1]["input"]["path"], "/etc/fstab");
        assert_eq!(reply[2]["id"], "toolu_02");
        assert_eq!(reply[2]["input"], json!({}));
        let results = msgs[2]["content"].as_array().unwrap();
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "toolu_01");
        assert_eq!(results[0]["content"], "/dev/sda1 / ext4");
        assert!(
            results[1].get("content").is_none(),
            "empty output sends no content"
        );
        assert_eq!(results[1]["cache_control"]["type"], "ephemeral");
        assert!(results[0].get("cache_control").is_none());
        assert!(!b.to_string().contains("thinking_block"));
    }

    #[test]
    fn thinking_and_effort_follow_the_model() {
        // The generation before adaptive thinking gets neither setting.
        let b = body(&req("claude-haiku-4-5-20251001", Some("high")), None);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
        // `reasoning = "default"` sends nothing.
        let b = body(&req("claude-opus-5-5", None), None);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
        // The catalog's word beats the id.
        let facts = ModelFacts {
            max_output: Some(32_000),
            adaptive: Some(false),
            effort: Some(true),
        };
        let b = body(&req("claude-opus-5-5", Some("low")), Some(&facts));
        assert!(b.get("thinking").is_none());
        assert_eq!(b["output_config"]["effort"], "low");
        assert_eq!(b["max_tokens"], 32_000);
    }

    #[test]
    fn adaptive_thinking_starts_at_4_6() {
        for id in [
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-mythos-5",
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-haiku-5-5",
        ] {
            assert!(adaptive_by_id(id), "{id}");
        }
        for id in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-20250514",
            "claude-3-5-haiku-20241022",
            "claude-3-7-sonnet-latest",
            "gpt-5",
        ] {
            assert!(!adaptive_by_id(id), "{id}");
        }
    }

    fn run(sse: &str) -> (Vec<StreamDelta>, Vec<String>, bool) {
        let mut st = EventState::default();
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for data in super::super::parse::sse_data(sse) {
            if data.is_empty() {
                continue;
            }
            let (ds, end) = parse_event(&mut st, &data);
            out.extend(ds);
            match end {
                End::Open => {}
                End::Done => return (out, errors, true),
                End::Failed(e) => {
                    errors.push(e.to_string());
                    return (out, errors, false);
                }
            }
        }
        (out, errors, false)
    }

    #[test]
    fn a_stream_yields_text_thinking_calls_and_usage() {
        let sse = include_str!("../../fixtures/anthropic_messages.sse");
        let (deltas, errors, done) = run(sse);
        assert!(done && errors.is_empty(), "{errors:?}");
        let text: String = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello world");
        let thought: String = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Reasoning(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thought, "Looking at the disk.");
        let usage = deltas.iter().fold(Usage::default(), |u, d| match d {
            StreamDelta::Usage(x) => u.merge(*x),
            _ => u,
        });
        assert_eq!(
            usage,
            Usage {
                input_tokens: 18,
                output_tokens: 30,
                cached_tokens: 2,
                cache_write_tokens: 4,
            },
            "input counts the cache reads and writes"
        );
        let mut acc = ToolCallAccumulator::default();
        for d in &deltas {
            if let StreamDelta::ToolCall {
                id,
                name,
                arguments,
            } = d
            {
                acc.push(id, name, arguments);
            }
        }
        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str()),
            ("toolu_01", "fs_read")
        );
        assert_eq!(calls[0].arguments, "{\"path\": \"/etc/fstab\"}");
        assert_eq!(calls[1].name, "sys_info");
        assert_eq!(
            calls[1].arguments, "{}",
            "no arguments streamed is an empty object"
        );
        assert!(!deltas.contains(&StreamDelta::Truncated));
        assert!(
            !deltas
                .iter()
                .any(|d| matches!(d, StreamDelta::ReportedCost(_)))
        );
    }

    #[test]
    fn the_output_limit_a_refusal_and_an_error_are_told_apart() {
        let cut = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":5}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let (d, errors, done) = run(cut);
        assert!(done && errors.is_empty() && d.contains(&StreamDelta::Truncated));
        let full = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"model_context_window_exceeded\"},\"usage\":{\"output_tokens\":5}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let (d, errors, done) = run(full);
        assert!(
            done && errors.is_empty() && d.contains(&StreamDelta::Truncated),
            "a reply that filled the context window is cut short too"
        );
        let whole = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n";
        assert!(!run(whole).0.contains(&StreamDelta::Truncated));
        let refused = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\"}},\"usage\":{\"output_tokens\":5}}\n\n";
        let (d, errors, _) = run(refused);
        assert!(
            errors[0].contains("declined") && errors[0].contains("cyber"),
            "{errors:?}"
        );
        assert!(
            d.iter().any(|x| matches!(x, StreamDelta::Usage(_))),
            "the tokens spent before the refusal are still counted"
        );
        let failed = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let (_, errors, _) = run(failed);
        assert!(
            errors[0].contains("overloaded_error: Overloaded"),
            "{errors:?}"
        );
        let (_, errors, _) = run("data: not json\n\n");
        assert!(errors[0].contains("messages sse"), "{errors:?}");
    }

    #[test]
    fn the_catalog_carries_list_prices_and_facts() {
        let json = include_str!("../../fixtures/anthropic_models.json");
        let cat = parse_models_json(json).unwrap();
        assert_eq!(cat.models.len(), 2, "the retired model is left out");
        let opus = &cat.models[0];
        assert_eq!(opus.id, "claude-opus-5-5");
        assert_eq!(opus.name.as_deref(), Some("Claude Opus 5.5"));
        assert_eq!(opus.context_length, Some(1_000_000));
        assert!((opus.input_per_million.unwrap() - 4.0).abs() < 1e-9);
        assert!((opus.output_per_million.unwrap() - 20.0).abs() < 1e-9);
        assert!((opus.cache_read_per_million.unwrap() - 0.2).abs() < 1e-9);
        assert!((opus.cache_write_per_million.unwrap() - 5.0).abs() < 1e-9);
        assert_eq!((opus.tools, opus.chat), (Some(true), Some(true)));
        assert_eq!(opus.unusable(), None);
        assert_eq!(
            cat.facts["claude-opus-5-5"],
            ModelFacts {
                max_output: Some(128_000),
                adaptive: Some(true),
                effort: Some(true),
            }
        );
        let haiku = &cat.models[1];
        assert!(
            (haiku.input_per_million.unwrap() - 1.0).abs() < 1e-9,
            "priced by its undated id"
        );
        assert_eq!(
            cat.facts["claude-haiku-4-5"], cat.facts["claude-haiku-4-5-20251001"],
            "the alias shares the dated id's facts"
        );
        assert_eq!(cat.facts["claude-haiku-4-5"].adaptive, Some(false));
        assert_eq!(cat.facts["claude-haiku-4-5"].max_output, Some(64_000));
        assert_eq!(cat.next.as_deref(), Some("claude-3-opus-20240229"));
        let last = parse_models_json(r#"{"data":[],"has_more":false,"last_id":null}"#).unwrap();
        assert!(last.models.is_empty() && last.next.is_none());
        assert!(parse_models_json(r#"{"error":{}}"#).is_err());
    }

    #[test]
    fn prices_match_dated_ids_and_unknown_models_have_none() {
        assert_eq!(
            rates("claude-sonnet-4-5-20250929").map(|p| p.input),
            Some(3.0)
        );
        assert_eq!(
            rates("claude-opus-4-1-20250805").map(|p| p.output),
            Some(75.0)
        );
        assert_eq!(
            rates("claude-sonnet-4-20250514").map(|p| p.input),
            Some(3.0)
        );
        assert_eq!(
            rates("claude-3-5-haiku-20241022").map(|p| p.cache_read),
            Some(0.08)
        );
        assert_eq!(rates("Claude-Opus-5-5").map(|p| p.cache_write), Some(5.0));
        assert_eq!(
            rates("claude-opus-5").map(|p| p.input),
            Some(5.0),
            "not the 5.5 row"
        );
        assert_eq!(rates("claude-next-9"), None);
        assert_eq!(undated("claude-opus-5-5"), None);
        assert_eq!(
            undated("claude-haiku-4-5-20251001"),
            Some("claude-haiku-4-5")
        );
    }
}
