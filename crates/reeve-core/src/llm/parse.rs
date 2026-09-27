//! SSE parser for Chat Completions.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::llm::StreamDelta;
use crate::spend::Usage;

/// Parse a full SSE document into deltas.
pub fn parse_sse(body: &str) -> Result<Vec<StreamDelta>> {
    let (mut out, _) = parse_blocks(body)?;
    out.push(StreamDelta::Done);
    Ok(out)
}

/// Parse SSE blocks into deltas, and say whether the provider said the
/// answer is over (`[DONE]`). Nothing after that point is read. An error the
/// provider sends inside the stream is an `Err`: dropped, it would end the
/// turn as a normal, empty reply.
pub fn parse_blocks(body: &str) -> Result<(Vec<StreamDelta>, bool)> {
    let mut out = Vec::new();
    for data in sse_data(body) {
        if data == "[DONE]" {
            return Ok((out, true));
        }
        if data.is_empty() {
            continue;
        }
        push_chat(&mut out, &data)?;
    }
    Ok((out, false))
}

/// The provider's own words for an error object: `type: message`.
fn error_text(e: &Value) -> String {
    let msg = e
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| e.as_str())
        .unwrap_or("");
    let kind = e
        .get("type")
        .or_else(|| e.get("code"))
        .map(|k| match k {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    match (kind.is_empty(), msg.is_empty()) {
        (false, false) => format!("{kind}: {msg}"),
        (true, false) => msg.to_string(),
        (false, true) => kind,
        (true, true) => e.to_string(),
    }
}

fn mid_stream(e: &Value) -> Error {
    Error::Provider(format!("the provider failed mid-reply: {}", error_text(e)))
}

/// The provider stopped the reply itself, for a reason other than done or
/// the output limit.
fn refused(reason: &str) -> Error {
    Error::Provider(match reason {
        "context_length_exceeded" => {
            "the conversation is longer than the model's context window; start a new session".into()
        }
        other => format!("the provider stopped the reply: {other}"),
    })
}

/// The `data:` payload of each SSE block. Comments (keep-alives) and event
/// names are dropped: Chat Completions doesn't use them.
fn sse_data(body: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut data = String::new();
    let mut open = false;
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if open {
                blocks.push(std::mem::take(&mut data));
                open = false;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if open {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
            open = true;
        }
    }
    if open {
        blocks.push(data);
    }
    blocks
}

fn push_chat(out: &mut Vec<StreamDelta>, data: &str) -> Result<()> {
    let v: Value = serde_json::from_str(data)
        .map_err(|e| Error::Provider(format!("chat sse: {e}: {data}")))?;
    // OpenRouter reports an upstream failure as a chunk with `error`.
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        return Err(mid_stream(e));
    }
    if let Some(u) = usage_from(&v["usage"]) {
        out.push(StreamDelta::Usage(u));
    }
    if let Some(c) = reported_cost(&v) {
        out.push(StreamDelta::ReportedCost(c));
    }
    let Some(choice) = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    else {
        return Ok(());
    };
    let delta = &choice["delta"];
    if let Some(s) = delta.get("content").and_then(Value::as_str) {
        if !s.is_empty() {
            out.push(StreamDelta::Text(s.to_string()));
        }
    }
    if let Some(s) = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
    {
        if !s.is_empty() {
            out.push(StreamDelta::Reasoning(s.to_string()));
        }
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let func = &call["function"];
            let name = func.get("name").and_then(Value::as_str).unwrap_or("");
            // Some OpenAI-compatible servers send the arguments as an
            // object, not a JSON string.
            let arguments = match func.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(v @ Value::Object(_)) => v.to_string(),
                _ => String::new(),
            };
            out.push(StreamDelta::ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            });
        }
    }
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => out.push(StreamDelta::Truncated),
        Some("error") => {
            return Err(mid_stream(choice.get("error").unwrap_or(&Value::Null)));
        }
        Some(r @ ("content_filter" | "refusal" | "context_length_exceeded")) => {
            return Err(refused(r));
        }
        _ => {}
    }
    Ok(())
}

fn usage_from(v: &Value) -> Option<Usage> {
    if v.is_null() {
        return None;
    }
    let n = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
    let details = v.get("prompt_tokens_details");
    let input = n(v.get("prompt_tokens"));
    let output = n(v.get("completion_tokens"));
    let cached = n(details.and_then(|d| d.get("cached_tokens")));
    // OpenRouter: `prompt_tokens_details.cache_write_tokens`. Some
    // Anthropic-shaped proxies pass `cache_creation_input_tokens` through.
    let written = n(details
        .and_then(|d| d.get("cache_write_tokens"))
        .or_else(|| v.get("cache_creation_input_tokens")));
    if input == 0 && output == 0 && cached == 0 && written == 0 {
        return None;
    }
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: cached,
        cache_write_tokens: written,
    })
}

fn reported_cost(v: &Value) -> Option<f64> {
    v.get("usage")
        .and_then(|u| u.get("cost").or_else(|| u.get("total_cost")))
        .and_then(Value::as_f64)
        .or_else(|| v.get("cost").and_then(Value::as_f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_reported() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"half\"},\"finish_reason\":\"length\"}]}\n\n";
        assert!(parse_sse(sse).unwrap().contains(&StreamDelta::Truncated));
    }

    #[test]
    fn a_normal_stop_is_not_truncation() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"all\"},\"finish_reason\":\"stop\"}]}\n\n";
        assert!(!parse_sse(sse).unwrap().contains(&StreamDelta::Truncated));
    }

    #[test]
    fn keep_alive_comments_are_ignored() {
        let sse =
            ": OPENROUTER PROCESSING\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let d = parse_sse(sse).unwrap();
        assert_eq!(d[0], StreamDelta::Text("x".into()));
    }

    #[test]
    fn errors_sent_mid_stream_are_errors() {
        let sse = "data: {\"error\":{\"code\":502,\"message\":\"upstream died\"}}\n\n";
        let e = parse_sse(sse).unwrap_err().to_string();
        assert!(e.contains("upstream died"), "{e}");
    }
}
