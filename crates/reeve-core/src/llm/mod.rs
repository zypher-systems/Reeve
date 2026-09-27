//! Inference providers: OpenRouter and any OpenAI-compatible endpoint.
//!
//! Copied from Ryter's `llm/` and trimmed to Chat Completions, which every
//! connection Reeve supports speaks.

mod http;
mod parse;

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;

use crate::error::Result;
use crate::spend::Usage;

pub use http::HttpProvider;
pub use parse::parse_sse;

/// A boxed stream of inference deltas.
pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<StreamDelta>> + Send>>;

/// One step of a streamed completion.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamDelta {
    /// Assistant text.
    Text(String),
    /// Reasoning / thinking text.
    Reasoning(String),
    /// Incremental tool call.
    ToolCall {
        /// Provider id (may be empty on a later delta).
        id: String,
        /// Function name (may be empty on a later delta).
        name: String,
        /// Arguments JSON fragment.
        arguments: String,
    },
    /// Token usage (usually on the last chunk).
    Usage(Usage),
    /// Provider-reported USD (OpenRouter's `usage.cost`). Wins over the price book.
    ReportedCost(f64),
    /// The model hit the output-token ceiling mid-answer.
    Truncated,
    /// Stream finished.
    Done,
}

/// A chat message sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// `system`, `user`, `assistant`, or `tool`.
    pub role: String,
    /// Text body.
    #[serde(default)]
    pub content: String,
    /// Tool-call id when role is `tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Assistant tool calls (when the model invoked tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AssistantToolCall>>,
}

impl Message {
    /// A plain message with no tool fields.
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

/// One tool call on an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantToolCall {
    /// Provider call id.
    pub id: String,
    /// Function name.
    pub name: String,
    /// JSON arguments (full object as a string).
    pub arguments: String,
}

/// Tool advertised to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Function name.
    pub name: String,
    /// Human description.
    pub description: String,
    /// JSON Schema object for arguments.
    pub parameters: serde_json::Value,
}

/// A completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Model id.
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// Conversation.
    pub messages: Vec<Message>,
    /// Tools. Empty means none.
    pub tools: Vec<ToolSpec>,
    /// Max output tokens.
    pub max_tokens: Option<u32>,
    /// Reasoning effort (`low`, `medium`, `high`), sent to OpenRouter only.
    /// `None` sends nothing, and then some models reason without limit.
    pub reasoning: Option<String>,
}

/// An entry from `GET /models`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Provider model id.
    pub id: String,
    /// Display name, when the catalog has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Context window if advertised.
    #[serde(default)]
    pub context_length: Option<u64>,
    /// USD per million input tokens.
    #[serde(default)]
    pub input_per_million: Option<f64>,
    /// USD per million output tokens.
    #[serde(default)]
    pub output_per_million: Option<f64>,
    /// USD per million cache-read tokens (OpenRouter `input_cache_read`).
    #[serde(default)]
    pub cache_read_per_million: Option<f64>,
    /// USD per million cache-write tokens (OpenRouter `input_cache_write`).
    #[serde(default)]
    pub cache_write_per_million: Option<f64>,
    /// Whether the model accepts tools, when the catalog says. An operator
    /// model that cannot call tools cannot do anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
}

impl ModelInfo {
    /// Catalog row with only an id.
    pub fn named(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }
}

/// Reassembles streamed tool calls.
///
/// Providers send a call's id and name once and then its arguments in
/// fragments that carry no id. An id-less fragment therefore continues the
/// most recent call.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: Vec<AssistantToolCall>,
}

impl ToolCallAccumulator {
    /// Fold one `StreamDelta::ToolCall` in.
    pub fn push(&mut self, id: &str, name: &str, arguments: &str) {
        let target = if id.is_empty() {
            self.calls.last_mut()
        } else {
            self.calls.iter_mut().find(|c| c.id == id)
        };
        match target {
            Some(call) => {
                if !name.is_empty() {
                    call.name = name.to_string();
                }
                call.arguments.push_str(arguments);
            }
            None => self.calls.push(AssistantToolCall {
                id: if id.is_empty() {
                    format!("call_{}", self.calls.len() + 1)
                } else {
                    id.to_string()
                },
                name: name.to_string(),
                arguments: arguments.to_string(),
            }),
        }
    }

    /// Completed calls in arrival order. Nameless fragments are dropped.
    pub fn finish(self) -> Vec<AssistantToolCall> {
        self.calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .collect()
    }

    /// Nothing accumulated yet.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

/// Streaming inference backend.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stream a completion.
    async fn stream(&self, req: CompletionRequest) -> Result<DeltaStream>;
    /// List models on this connection.
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
}

/// Yields pre-recorded streams. Each `stream` call consumes the next scripted turn.
pub struct ReplayProvider {
    turns: Mutex<VecDeque<Vec<StreamDelta>>>,
    models: Vec<ModelInfo>,
    repeat_last: bool,
}

impl ReplayProvider {
    /// Replay these deltas on every `stream` call.
    pub fn new(deltas: Vec<StreamDelta>) -> Self {
        Self {
            turns: Mutex::new(VecDeque::from([deltas])),
            models: Vec::new(),
            repeat_last: true,
        }
    }

    /// One recorded SSE document, reused.
    pub fn from_sse(sse: &str) -> Result<Self> {
        Ok(Self::new(parse_sse(sse)?))
    }

    /// Distinct turns for an agent loop (tool call then final answer, …).
    pub fn scripted(turns: Vec<Vec<StreamDelta>>) -> Self {
        Self {
            turns: Mutex::new(VecDeque::from(turns)),
            models: Vec::new(),
            repeat_last: false,
        }
    }
}

#[async_trait]
impl Provider for ReplayProvider {
    async fn stream(&self, _req: CompletionRequest) -> Result<DeltaStream> {
        let mut q = self.turns.lock().expect("replay mutex");
        let deltas = if self.repeat_last && q.len() == 1 {
            q.front().cloned().unwrap_or_default()
        } else {
            q.pop_front().unwrap_or_default()
        };
        drop(q);
        Ok(Box::pin(futures_util::stream::iter(
            deltas.into_iter().map(Ok),
        )))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self.models.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[test]
    fn chat_fixture_streams_text_usage_and_cost() {
        let sse = include_str!("../../fixtures/chat_completions.sse");
        let deltas = parse_sse(sse).unwrap();
        let text: String = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello world");
        let usage = deltas.iter().find_map(|d| match d {
            StreamDelta::Usage(u) => Some(*u),
            _ => None,
        });
        assert_eq!(
            usage,
            Some(Usage {
                input_tokens: 12,
                output_tokens: 3,
                cached_tokens: 2,
                cache_write_tokens: 4,
            })
        );
        assert!(deltas.contains(&StreamDelta::ReportedCost(0.000123)));
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, StreamDelta::Reasoning(_)))
        );
        assert!(deltas.iter().any(|d| matches!(d, StreamDelta::Done)));
    }

    #[test]
    fn accumulator_continues_id_less_fragments() {
        let mut a = ToolCallAccumulator::default();
        a.push("c1", "fs_read", "");
        a.push("", "", "{\"path\":");
        a.push("", "", "\"/etc/fstab\"}");
        a.push("c2", "logs_query", "{\"unit\":\"sshd\"}");
        let calls = a.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, "{\"path\":\"/etc/fstab\"}");
        assert_eq!(calls[1].name, "logs_query");
    }

    #[tokio::test]
    async fn replay_provider_yields_fixture() {
        let sse = include_str!("../../fixtures/chat_completions.sse");
        let p = ReplayProvider::from_sse(sse).unwrap();
        let mut s = p
            .stream(CompletionRequest {
                model: "x".into(),
                system: None,
                messages: vec![],
                tools: vec![],
                max_tokens: None,
                reasoning: None,
            })
            .await
            .unwrap();
        let mut n = 0;
        while let Some(d) = s.next().await {
            d.unwrap();
            n += 1;
        }
        assert!(n >= 3);
    }
}
