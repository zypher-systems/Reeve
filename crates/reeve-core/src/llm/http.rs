//! Live HTTP provider (Chat Completions over SSE).

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Duration;

use crate::config::ConnectionConfig;
use crate::error::{Error, Result};
use crate::llm::{CompletionRequest, DeltaStream, ModelInfo, Provider, StreamDelta, ToolSpec};

/// How long to wait for the connection itself.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle bound for a local model server, which may load weights first.
const LOCAL_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a socket may go silent before it is considered dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest a stream may go without a real delta. Keep-alive comments reset
/// the socket's read timeout, so a request stuck in a provider's queue never
/// trips it; this bound does.
const STALL_TIMEOUT: Duration = Duration::from_secs(300);
/// The same bound for a local server.
const LOCAL_STALL_TIMEOUT: Duration = Duration::from_secs(900);
/// Attempts after the first for a retryable failure.
const MAX_RETRIES: u32 = 3;
/// First backoff step; doubles per attempt.
const BACKOFF_BASE: Duration = Duration::from_millis(500);
/// Ceiling on one backoff wait.
const BACKOFF_CAP: Duration = Duration::from_secs(20);

/// Statuses worth trying again: rate limits, overload, and gateway noise.
fn is_retryable_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
}

fn is_retryable_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

/// `Retry-After` in seconds, when the provider sent one, clamped.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get("retry-after")?.to_str().ok()?;
    v.trim()
        .parse::<u64>()
        .ok()
        .map(|s| Duration::from_secs(s.min(BACKOFF_CAP.as_secs())))
}

/// Exponential backoff with a little jitter.
fn backoff(attempt: u32) -> Duration {
    let step = BACKOFF_BASE
        .saturating_mul(1u32 << attempt.min(5))
        .min(BACKOFF_CAP);
    step.saturating_add(Duration::from_millis(u64::from(jitter_ms())))
}

fn jitter_ms() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 250) as u16
}

/// reqwest-backed provider for OpenRouter and OpenAI-compatible endpoints.
pub struct HttpProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    kind: String,
    local: bool,
}

impl HttpProvider {
    /// New client. Does not touch the network until `stream` / `list_models`.
    pub fn new(conn: &ConnectionConfig, api_key: String) -> Self {
        let local = conn.is_local();
        Self {
            client: reqwest::Client::builder()
                // A streamed turn has no useful total deadline; bound the gap
                // between chunks instead.
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(if local {
                    LOCAL_IDLE_TIMEOUT
                } else {
                    IDLE_TIMEOUT
                })
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: conn.base_url.trim_end_matches('/').to_string(),
            api_key,
            kind: conn.kind.clone(),
            local,
        }
    }

    /// Check that the key works, and say what the provider knows about it.
    /// OpenRouter has a key endpoint (usage and limit); elsewhere, an
    /// authenticated `/models` call is the check.
    pub async fn verify(&self) -> Result<String> {
        if self.kind == "openrouter" {
            let resp = self
                .client
                .get(format!("{}/key", self.base_url))
                .headers(self.headers()?)
                .send()
                .await
                .map_err(|e| Error::Provider(e.to_string()))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(Error::Provider(key_error(status.as_u16(), &text)));
            }
            return Ok(describe_openrouter_key(&text));
        }
        let n = self.list_models().await.map_err(|e| {
            Error::Provider(match e {
                Error::Provider(m) if m.contains("401") || m.contains("403") => {
                    "the provider rejected this key".into()
                }
                other => other.to_string(),
            })
        })?;
        Ok(match n.len() {
            1 => "key works · 1 model".to_string(),
            k => format!("key works · {k} models"),
        })
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // A keyless local server gets no Authorization header rather than
        // an empty `Bearer `, which some servers reject.
        if !self.api_key.is_empty() {
            let auth = format!("Bearer {}", self.api_key);
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth).map_err(|e| Error::Provider(e.to_string()))?,
            );
        }
        if self.kind == "openrouter" {
            h.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://github.com/zypher-systems/reeve"),
            );
            h.insert("X-Title", HeaderValue::from_static("Reeve"));
        }
        Ok(h)
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn body(&self, req: &CompletionRequest) -> Value {
        let mut body = chat_body(req);
        // OpenRouter normalizes one reasoning setting across its models, and
        // reports the call's real cost when asked. Other OpenAI-compatible
        // servers may reject unknown fields.
        if self.kind == "openrouter" {
            if let Some(effort) = &req.reasoning {
                body["reasoning"] = json!({ "effort": effort });
            }
            body["usage"] = json!({ "include": true });
        }
        body
    }
}

#[async_trait]
impl Provider for HttpProvider {
    async fn stream(&self, req: CompletionRequest) -> Result<DeltaStream> {
        let headers = self.headers()?;
        let body = self.body(&req);
        let mut last = String::new();
        // Only the opening request is retried. Once deltas have been handed to
        // the caller, a retry would duplicate text they already have.
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt - 1)).await;
            }
            let sent = self
                .client
                .post(self.endpoint())
                .headers(headers.clone())
                .json(&body)
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(e) if e.is_connect() && self.local => {
                    return Err(Error::Provider(format!(
                        "could not reach the local model server at {} — is it running? ({e})",
                        self.base_url
                    )));
                }
                Err(e) if is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    last = e.to_string();
                    continue;
                }
                Err(e) => return Err(Error::Provider(e.to_string())),
            };
            let status = resp.status();
            if status.is_success() {
                let stall = if self.local {
                    LOCAL_STALL_TIMEOUT
                } else {
                    STALL_TIMEOUT
                };
                return Ok(Box::pin(sse_delta_stream(resp.bytes_stream(), stall)));
            }
            let wait = retry_after(resp.headers());
            let text = resp.text().await.unwrap_or_default();
            last = format!("http {status}: {text}");
            if !is_retryable_status(status.as_u16()) || attempt == MAX_RETRIES {
                return Err(Error::Provider(last));
            }
            if let Some(w) = wait {
                tokio::time::sleep(w).await;
            }
        }
        Err(Error::Provider(format!(
            "{last} (after {MAX_RETRIES} retries)"
        )))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let resp = self
            .client
            .get(format!("{}/models", self.base_url))
            .headers(self.headers()?)
            .send()
            .await
            .map_err(|e| Error::Provider(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Provider(e.to_string()))?;
        if !status.is_success() {
            return Err(Error::Provider(format!("http {status}: {text}")));
        }
        parse_models_json(&text)
    }
}

fn sse_delta_stream<E: std::fmt::Display>(
    byte_stream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin + 'static,
    stall: Duration,
) -> impl Stream<Item = Result<StreamDelta>> + Send {
    futures_util::stream::unfold(
        StreamState {
            byte_stream,
            raw: Vec::new(),
            buf: String::new(),
            pending: VecDeque::new(),
            done: false,
            stall,
            progress: tokio::time::Instant::now(),
        },
        |mut st| async move {
            loop {
                if let Some(d) = st.pending.pop_front() {
                    if d.is_err() {
                        st.done = true;
                        st.pending.clear();
                    }
                    return Some((d, st));
                }
                if st.done {
                    return None;
                }
                let next =
                    tokio::time::timeout_at(st.progress + st.stall, st.byte_stream.next()).await;
                let Ok(next) = next else {
                    st.done = true;
                    return Some((
                        Err(Error::Provider(format!(
                            "the provider sent nothing but keep-alives for {}s; the request \
                             looks stuck on its side. Try again, or another model.",
                            st.stall.as_secs()
                        ))),
                        st,
                    ));
                };
                match next {
                    Some(Ok(chunk)) => {
                        st.raw.extend_from_slice(&chunk);
                        let text = take_utf8(&mut st.raw);
                        st.buf.push_str(&text.replace("\r\n", "\n"));
                        drain_sse(&mut st);
                    }
                    Some(Err(e)) => {
                        st.done = true;
                        let msg = e.to_string();
                        // reqwest says "error decoding response body" when the
                        // read timeout fires between chunks.
                        let msg = if msg.contains("decoding response body") {
                            format!("the provider's stream went silent ({msg})")
                        } else {
                            msg
                        };
                        return Some((Err(Error::Provider(msg)), st));
                    }
                    None => {
                        let rest =
                            String::from_utf8_lossy(&std::mem::take(&mut st.raw)).into_owned();
                        st.buf.push_str(&rest);
                        let rest = std::mem::take(&mut st.buf);
                        if !rest.trim().is_empty() {
                            push_block(&mut st, &rest);
                        }
                        st.done = true;
                        if let Some(d) = st.pending.pop_front() {
                            return Some((d, st));
                        }
                        return None;
                    }
                }
            }
        },
    )
}

struct StreamState<S> {
    byte_stream: S,
    /// Bytes not yet decoded: a character split across two chunks waits
    /// here for its other half.
    raw: Vec<u8>,
    buf: String,
    pending: VecDeque<Result<StreamDelta>>,
    done: bool,
    stall: Duration,
    /// When the last real delta arrived.
    progress: tokio::time::Instant,
}

/// Decode the whole characters at the front of `raw`, leaving an incomplete
/// one at the end for the next chunk.
fn take_utf8(raw: &mut Vec<u8>) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => {
            let s = s.to_string();
            raw.clear();
            s
        }
        Err(e) if e.error_len().is_none() => {
            let tail = raw.split_off(e.valid_up_to());
            let s = String::from_utf8_lossy(raw).into_owned();
            *raw = tail;
            s
        }
        Err(_) => {
            let s = String::from_utf8_lossy(raw).into_owned();
            raw.clear();
            s
        }
    }
}

fn drain_sse<S>(st: &mut StreamState<S>) {
    while !st.done
        && let Some(idx) = st.buf.find("\n\n")
    {
        let block = st.buf[..idx + 2].to_string();
        st.buf = st.buf[idx + 2..].to_string();
        push_block(st, &block);
    }
}

fn push_block<S>(st: &mut StreamState<S>, block: &str) {
    match super::parse::parse_blocks(block) {
        Ok((ds, terminal)) => {
            if !ds.is_empty() {
                st.progress = tokio::time::Instant::now();
            }
            st.pending.extend(ds.into_iter().map(Ok));
            // `[DONE]`: the answer is whole. Waiting for the socket to close
            // spins until the idle timeout on servers that keep it open.
            if terminal {
                st.pending.push_back(Ok(StreamDelta::Done));
                st.done = true;
            }
        }
        Err(e) => st.pending.push_back(Err(e)),
    }
}

fn chat_body(req: &CompletionRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(sys) = &req.system {
        messages.push(json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        let mut obj = json!({"role": m.role, "content": m.content});
        if let Some(id) = &m.tool_call_id {
            obj["tool_call_id"] = json!(id);
        }
        if let Some(calls) = &m.tool_calls {
            obj["tool_calls"] = json!(
                calls
                    .iter()
                    .map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        messages.push(obj);
    }
    if wants_cache_marks(&req.model) {
        // System prompt, then the newest user/assistant message. Tool results
        // stay plain strings: not every route accepts blocks in that role.
        if let Some(first) = messages.first_mut() {
            if first["role"] == "system" {
                mark_for_cache(first);
            }
        }
        if let Some(last) = messages
            .iter_mut()
            .rev()
            .find(|m| m["role"] == "user" || m["role"] == "assistant")
        {
            if last["content"].as_str().is_some_and(|c| !c.is_empty()) {
                mark_for_cache(last);
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if !req.tools.is_empty() {
        body["tools"] = tools_openai(&req.tools);
    }
    body
}

/// Put a cache breakpoint on a message. An agent loop resends the whole
/// conversation every round; with the breakpoint rolling forward, everything
/// up to the previous round is read from cache.
fn mark_for_cache(msg: &mut Value) {
    let content = &mut msg["content"];
    if let Some(text) = content.as_str().map(str::to_string) {
        *content = json!([{ "type": "text", "text": text }]);
    }
    if let Some(block) = content.as_array_mut().and_then(|a| a.last_mut()) {
        block["cache_control"] = json!({ "type": "ephemeral" });
    }
}

/// Anthropic models reached through OpenRouter cache only when a block is
/// marked. Other providers cache automatically and get plain content.
fn wants_cache_marks(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("anthropic/") || m.contains("claude")
}

fn tools_openai(tools: &[ToolSpec]) -> Value {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

fn key_error(status: u16, body: &str) -> String {
    match status {
        401 | 403 => "the provider rejected this key".into(),
        _ => format!(
            "http {status}: {}",
            body.chars().take(200).collect::<String>()
        ),
    }
}

/// `key works · $1.23 used of $20.00` from OpenRouter's `GET /key`.
fn describe_openrouter_key(text: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let d = &v["data"];
    let used = d["usage"].as_f64();
    let limit = d["limit"].as_f64();
    let mut out = "key works".to_string();
    match (used, limit) {
        (Some(u), Some(l)) => out.push_str(&format!(" · ${u:.2} used of ${l:.2}")),
        (Some(u), None) => out.push_str(&format!(" · ${u:.2} used, no limit")),
        _ => {}
    }
    if d["is_free_tier"].as_bool() == Some(true) {
        out.push_str(" · free tier");
    }
    out
}

/// Per-token price string → per-million, or `None`. OpenRouter uses `"-1"`
/// for "varies by request" (routers), which is not a price.
fn per_million(v: Option<&Value>) -> Option<f64> {
    let per_token = match v? {
        Value::String(s) => s.parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (per_token >= 0.0).then_some(per_token * 1_000_000.0)
}

pub(crate) fn parse_models_json(text: &str) -> Result<Vec<ModelInfo>> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| Error::Provider(format!("models json: {e}")))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Provider("models: missing data[]".into()))?;
    Ok(data
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?;
            let p = m.get("pricing");
            let price = |k: &str| per_million(p.and_then(|p| p.get(k)));
            Some(ModelInfo {
                id: id.to_string(),
                name: m.get("name").and_then(Value::as_str).map(str::to_string),
                context_length: m
                    .get("context_length")
                    .or_else(|| m.get("context_window"))
                    .and_then(Value::as_u64),
                input_per_million: price("prompt").or_else(|| price("input")),
                output_per_million: price("completion").or_else(|| price("output")),
                cache_read_per_million: price("input_cache_read"),
                cache_write_per_million: price("input_cache_write"),
                tools: m
                    .get("supported_parameters")
                    .and_then(Value::as_array)
                    .map(|p| p.iter().any(|x| x.as_str() == Some("tools"))),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    type Chunk = std::result::Result<Bytes, std::io::Error>;

    fn chunks(parts: &[&[u8]]) -> Vec<Chunk> {
        parts
            .iter()
            .map(|p| Ok(Bytes::copy_from_slice(p)))
            .collect()
    }

    async fn collect(
        s: impl Stream<Item = Result<StreamDelta>> + Send,
        wait: Duration,
    ) -> Option<Vec<std::result::Result<StreamDelta, String>>> {
        let all = s.map(|d| d.map_err(|e| e.to_string())).collect::<Vec<_>>();
        tokio::time::timeout(wait, all).await.ok()
    }

    #[tokio::test]
    async fn a_finished_answer_ends_without_waiting_for_the_socket() {
        let body: &[u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let open = stream::iter(chunks(&[body])).chain(stream::pending());
        let got = collect(
            sse_delta_stream(Box::pin(open), Duration::from_secs(60)),
            Duration::from_secs(2),
        )
        .await
        .expect("waited for the socket");
        assert_eq!(
            got,
            vec![Ok(StreamDelta::Text("hi".into())), Ok(StreamDelta::Done)]
        );
    }

    #[tokio::test]
    async fn keep_alives_alone_hit_the_stall_deadline() {
        let pings = stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((
                Ok::<_, std::io::Error>(Bytes::from_static(b": OPENROUTER PROCESSING\n\n")),
                (),
            ))
        });
        let got = collect(
            sse_delta_stream(Box::pin(pings), Duration::from_millis(300)),
            Duration::from_secs(3),
        )
        .await
        .expect("the stall deadline never fired");
        assert_eq!(got.len(), 1);
        assert!(got[0].as_ref().unwrap_err().contains("keep-alives"));
    }

    #[tokio::test]
    async fn a_character_split_across_chunks_arrives_whole() {
        let body =
            "data: {\"choices\":[{\"delta\":{\"content\":\"café 日本\"}}]}\n\ndata: [DONE]\n\n";
        let bytes = body.as_bytes();
        let cut = body.find('é').unwrap() + 1;
        let cut2 = body.find('本').unwrap() + 2;
        let parts = chunks(&[&bytes[..cut], &bytes[cut..cut2], &bytes[cut2..]]);
        let got = collect(
            sse_delta_stream(Box::pin(stream::iter(parts)), Duration::from_secs(60)),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(got[0], Ok(StreamDelta::Text("café 日本".into())));
    }

    #[test]
    fn openrouter_gets_reasoning_and_cost_others_do_not() {
        let req = CompletionRequest {
            model: "z-ai/glm-5".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: Some(16),
            reasoning: Some("medium".into()),
        };
        let or = crate::config::connection_template("openrouter").unwrap();
        let body = HttpProvider::new(&or, "k".into()).body(&req);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["usage"]["include"], true);
        let local = crate::config::connection_template("ollama").unwrap();
        let body = HttpProvider::new(&local, String::new()).body(&req);
        assert!(body.get("reasoning").is_none() && body.get("usage").is_none());
    }

    #[test]
    fn claude_over_openrouter_is_marked_for_cache() {
        let req = CompletionRequest {
            model: "anthropic/claude-sonnet-4.6".into(),
            system: Some("sys".into()),
            messages: vec![crate::llm::Message::new("user", "hi")],
            tools: vec![],
            max_tokens: None,
            reasoning: None,
        };
        let wire = chat_body(&req).to_string();
        assert_eq!(wire.matches("cache_control").count(), 2, "{wire}");
        let other = CompletionRequest {
            model: "openai/gpt-5".into(),
            ..req
        };
        assert!(!chat_body(&other).to_string().contains("cache_control"));
    }

    #[test]
    fn retryable_statuses_are_the_transient_ones() {
        for code in [408, 425, 429, 500, 502, 503, 504, 529] {
            assert!(is_retryable_status(code), "{code}");
        }
        for code in [200, 400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(code), "{code}");
        }
    }

    #[test]
    fn retry_after_is_clamped() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("99999"));
        assert_eq!(retry_after(&h), Some(BACKOFF_CAP));
    }

    #[test]
    fn openrouter_catalog_carries_cache_prices() {
        let json = include_str!("../../fixtures/openrouter_models.json");
        let models = parse_models_json(json).unwrap();
        let sonnet = &models[0];
        assert_eq!(sonnet.id, "anthropic/claude-sonnet-4.6");
        assert!((sonnet.input_per_million.unwrap() - 3.0).abs() < 1e-9);
        assert!((sonnet.cache_read_per_million.unwrap() - 0.3).abs() < 1e-9);
        assert!((sonnet.cache_write_per_million.unwrap() - 3.75).abs() < 1e-9);
        assert_eq!(sonnet.tools, Some(true));
        assert_eq!(models[1].cache_read_per_million, None);
        assert_eq!(models[2].input_per_million, None);
    }

    #[test]
    fn openrouter_key_info_reads_well() {
        let j =
            r#"{"data":{"label":"sk-or-v1-abc","usage":1.234,"limit":20,"is_free_tier":false}}"#;
        assert_eq!(
            describe_openrouter_key(j),
            "key works · $1.23 used of $20.00"
        );
        let j = r#"{"data":{"usage":0.5,"limit":null}}"#;
        assert_eq!(
            describe_openrouter_key(j),
            "key works · $0.50 used, no limit"
        );
        assert_eq!(key_error(401, "nope"), "the provider rejected this key");
    }

    #[test]
    fn variable_router_prices_are_not_prices() {
        let json =
            r#"{"data":[{"id":"openrouter/auto","pricing":{"prompt":"-1","completion":"-1"}}]}"#;
        let m = &parse_models_json(json).unwrap()[0];
        assert_eq!(m.input_per_million, None);
    }
}
