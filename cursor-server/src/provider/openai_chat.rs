use std::collections::{btree_map::Entry, BTreeMap};

use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::{config::ProviderConfig, model::Usage, prompting::ModelRequest, Error};

use super::{FinishReason, Provider, ProviderStream, ResponseEvent};

pub struct OpenAiChatProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

impl OpenAiChatProvider {
    pub fn new(client: reqwest::Client, config: ProviderConfig) -> Self {
        Self { client, config }
    }
}

impl Provider for OpenAiChatProvider {
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ProviderStream {
        let client = self.client.clone();
        let config = self.config.clone();
        Box::pin(try_stream! {
            let messages = openai_chat_messages(&request.messages);
            let body = json!({
                "model": request.model,
                "messages": messages,
                "tools": request.tools.iter().map(|tool| json!({"type":"function","function":{
                    "name": tool.name, "description": tool.description, "parameters": tool.input_schema
                }})).collect::<Vec<_>>(),
                "stream": true,
                "stream_options": {"include_usage": true}
            });
            let response = client.post(format!("{}/chat/completions", config.base_url))
                .bearer_auth(&config.api_key).json(&body).send().await?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await?;
                Err(Error::Provider(format!("OpenAI Chat {status}: {text}")))?;
                return;
            }
            yield ResponseEvent::Start { model_call_id: request.model_call_id };
            let mut source = response.bytes_stream().eventsource();
            let mut text_open = false;
            let mut thinking_open = false;
            let mut tools: BTreeMap<usize, (String, String)> = BTreeMap::new();
            let mut finish = None;
            loop {
                let event = tokio::select! {
                    _ = cancellation.cancelled() => {
                        yield ResponseEvent::Done(FinishReason::Aborted);
                        return;
                    }
                    event = source.next() => event,
                };
                let Some(event) = event else { break };
                let event = event.map_err(|error| Error::Provider(format!("OpenAI Chat SSE: {error}")))?;
                if event.data == "[DONE]" { break; }
                let value: Value = serde_json::from_str(&event.data)?;
                if let Some(usage) = value.get("usage").filter(|value| !value.is_null()) {
                    yield ResponseEvent::Usage(openai_usage(usage));
                }
                let Some(choice) = value.get("choices").and_then(Value::as_array).and_then(|values| values.first()) else { continue; };
                let delta = choice.get("delta").unwrap_or(&Value::Null);
                if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                    if !thinking_open { thinking_open = true; yield ResponseEvent::ThinkingStart; }
                    yield ResponseEvent::ThinkingDelta(reasoning.into());
                }
                if let Some(content) = delta.get("content").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                    if thinking_open { thinking_open = false; yield ResponseEvent::ThinkingEnd; }
                    if !text_open { text_open = true; yield ResponseEvent::TextStart; }
                    yield ResponseEvent::TextDelta(content.into());
                }
                if let Some(tool_deltas) = delta.get("tool_calls").and_then(Value::as_array) {
                    for tool in tool_deltas {
                        let index = tool.get("index").and_then(Value::as_u64)
                            .ok_or_else(|| Error::Provider("OpenAI Chat tool delta is missing index".into()))? as usize;
                        let id = tool.get("id").and_then(Value::as_str);
                        let function = tool.get("function").unwrap_or(&Value::Null);
                        let name = function.get("name").and_then(Value::as_str);
                        match tools.entry(index) {
                            Entry::Vacant(entry) => {
                                let id = id.ok_or_else(|| Error::Provider("OpenAI Chat tool start is missing id".into()))?;
                                let name = name.ok_or_else(|| Error::Provider("OpenAI Chat tool start is missing name".into()))?;
                                entry.insert((id.into(), name.into()));
                                yield ResponseEvent::ToolCallStart { index, call_id: id.into(), name: name.into() };
                            }
                            Entry::Occupied(mut entry) => {
                                if let Some(id) = id { entry.get_mut().0.push_str(id); }
                                if let Some(name) = name { entry.get_mut().1.push_str(name); }
                            }
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                            yield ResponseEvent::ToolCallArgumentsDelta { index, delta: arguments.into() };
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish = Some(map_finish(reason)?);
                }
            }
            if thinking_open { yield ResponseEvent::ThinkingEnd; }
            if text_open { yield ResponseEvent::TextEnd; }
            for index in tools.keys().copied() { yield ResponseEvent::ToolCallEnd { index }; }
            yield ResponseEvent::Done(finish.ok_or_else(|| Error::Provider("OpenAI Chat stream ended without finish_reason".into()))?);
        })
    }
}

fn openai_chat_messages(messages: &[crate::prompting::ProviderMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut output = Map::new();
            output.insert("role".into(), Value::String(message.role.clone()));
            output.insert("content".into(), message.content.clone());
            if let Some(thinking) = &message.thinking {
                output.insert("reasoning_content".into(), Value::String(thinking.clone()));
            }
            if let Some(tool_call_id) = &message.tool_call_id {
                output.insert("tool_call_id".into(), Value::String(tool_call_id.clone()));
            }
            if let Some(tool_calls) = &message.tool_calls {
                output.insert("tool_calls".into(), Value::Array(tool_calls.clone()));
            }
            Value::Object(output)
        })
        .collect()
}

fn map_finish(value: &str) -> crate::Result<FinishReason> {
    match value {
        "tool_calls" | "function_call" => Ok(FinishReason::ToolUse),
        "length" => Ok(FinishReason::Length),
        "stop" => Ok(FinishReason::Stop),
        other => Err(Error::Provider(format!(
            "unknown OpenAI Chat finish_reason: {other}"
        ))),
    }
}

pub(crate) fn openai_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: value
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::openai_chat_messages;
    use crate::prompting::ProviderMessage;
    use serde_json::{json, Value};

    #[test]
    fn assistant_thinking_is_encoded_as_reasoning_content() {
        let messages = openai_chat_messages(&[ProviderMessage {
            role: "assistant".into(),
            content: Value::String("visible answer".into()),
            thinking: Some("private reasoning".into()),
            tool_call_id: None,
            tool_calls: Some(vec![json!({
                "id": "call-1",
                "type": "function",
                "function": {"name": "Read", "arguments": "{}"}
            })]),
        }]);

        assert_eq!(messages[0]["content"], "visible answer");
        assert_eq!(messages[0]["reasoning_content"], "private reasoning");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call-1");
    }
}
