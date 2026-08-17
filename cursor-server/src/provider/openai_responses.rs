use async_stream::try_stream;
use base64::{engine::general_purpose::STANDARD, Engine};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::{
    config::ProviderConfig,
    model::{
        ContentPart, ModelInvocation, ModelLatency, ProjectedContent, ProjectedMessage, Role, Usage,
    },
    Error, Result,
};

use super::{
    merge_extra_params, recorder::recorded_headers, CallRecorder, FinishReason, ModelEvent,
    Provider, ProviderStream,
};

pub struct OpenAiResponsesProvider {
    client: reqwest::Client,
    config: ProviderConfig,
    recorder: Option<CallRecorder>,
}

impl OpenAiResponsesProvider {
    pub fn new(client: reqwest::Client, config: ProviderConfig) -> Self {
        Self {
            client,
            config,
            recorder: None,
        }
    }

    pub fn with_recorder(mut self, recorder: Option<CallRecorder>) -> Self {
        self.recorder = recorder;
        self
    }
}

impl Provider for OpenAiResponsesProvider {
    fn stream(
        &self,
        invocation: ModelInvocation,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ProviderStream {
        let client = self.client.clone();
        let config = self.config.clone();
        let recorder = self.recorder.clone();
        Box::pin(try_stream! {
            let ModelInvocation { call_id, request, .. } = invocation;
            let input = responses_input(&request.history)?;
            let mut body = json!({
                "model": request.model.model_id, "input": input, "stream": true,
                "instructions": request.prompt.instructions,
                "include": ["reasoning.encrypted_content"],
                "tools": request.prompt.tools.iter().map(|tool| json!({
                    "type":"function", "name":tool.name, "description":tool.description,
                    "parameters":tool.parameters, "strict":false
                })).collect::<Vec<_>>()
            });
            apply_model(&mut body, &request.model, config.max_output_tokens)?;
            merge_extra_params(&mut body, &request.model.extra_params)?;
            if let Some(recorder) = &recorder {
                recorder.request(recorded_headers(&config, &[("content-type", "application/json")]), &body).await?;
            }
            let request = client.post(format!("{}/responses", config.base_url))
                .bearer_auth(&config.api_key).headers(config.custom_headers.clone()).json(&body).send();
            let response = tokio::select! {
                _ = cancellation.cancelled() => return,
                response = request => response,
            };
            let response = response?;
            if let Some(recorder) = &recorder {
                recorder.response_headers(response.status().as_u16()).await?;
            }
            if !response.status().is_success() {
                let status = response.status(); let bytes = response.bytes().await?;
                if let Some(recorder) = &recorder { recorder.response_chunk(&bytes).await?; }
                let text = String::from_utf8_lossy(&bytes);
                Err(Error::Provider(format!("OpenAI Responses {status}: {text}")))?;
                return;
            }
            yield ModelEvent::Start { model_call_id: call_id };
            let chunk_recorder = recorder.clone();
            let chunks = response.bytes_stream()
                .map(|chunk| chunk.map_err(Error::from))
                .then(move |chunk| {
                    let recorder = chunk_recorder.clone();
                    async move {
                        let chunk = chunk?;
                        if let Some(recorder) = recorder { recorder.response_chunk(&chunk).await?; }
                        Ok::<_, Error>(chunk)
                    }
                });
            let source = chunks.eventsource();
            futures_util::pin_mut!(source);
            let mut text_open = false;
            let mut thinking_open = false;
            let mut tool_indices = std::collections::BTreeSet::new();
            let mut reasoning_items = Vec::new();
            let mut saw_tool = false;
            let mut terminal = false;
            loop {
                let event = tokio::select! {
                    _ = cancellation.cancelled() => { return; }
                    event = source.next() => event,
                };
                let Some(event) = event else { break };
                let event = event.map_err(|error| Error::Provider(format!("OpenAI Responses SSE: {error}")))?;
                if event.data == "[DONE]" { break; }
                let value: Value = serde_json::from_str(&event.data)?;
                let kind = value.get("type").and_then(Value::as_str).unwrap_or(&event.event);
                match kind {
                    "response.output_text.delta" => {
                        if thinking_open { thinking_open = false; yield ModelEvent::ThinkingEnd; }
                        if !text_open { text_open = true; yield ModelEvent::TextStart; }
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) { yield ModelEvent::TextDelta(delta.into()); }
                    }
                    "response.output_text.done" => {
                        if text_open { text_open = false; yield ModelEvent::TextEnd; }
                    }
                    "response.reasoning_summary_text.delta" => {
                        if !thinking_open { thinking_open = true; yield ModelEvent::ThinkingStart; }
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) { yield ModelEvent::ThinkingDelta(delta.into()); }
                    }
                    "response.reasoning_summary_text.done" => {
                        if thinking_open { thinking_open = false; yield ModelEvent::ThinkingEnd; }
                    }
                    "response.output_item.added" => {
                        let item = value.get("item").unwrap_or(&Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let index = required_u64(&value, "output_index")? as usize;
                            let call_id = required_string(item, "call_id")?.to_string();
                            let name = required_string(item, "name")?.to_string();
                            tool_indices.insert(index); saw_tool = true;
                            yield ModelEvent::ToolCallStart { index, call_id, name };
                        }
                    }
                    "response.output_item.done" => {
                        let item = value.get("item").unwrap_or(&Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                            reasoning_items.push(item.clone());
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let index = required_u64(&value, "output_index")? as usize;
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            yield ModelEvent::ToolCallArgumentsDelta { index, delta: delta.into() };
                        }
                    }
                    "response.function_call_arguments.done" => {
                        let index = required_u64(&value, "output_index")? as usize;
                        if tool_indices.remove(&index) { yield ModelEvent::ToolCallEnd { index }; }
                    }
                    "response.completed" => {
                        if let Some(usage) = value.pointer("/response/usage") { yield ModelEvent::Usage(responses_usage(usage)); }
                        if text_open || thinking_open || !tool_indices.is_empty() {
                            Err(Error::Provider("OpenAI Responses completed with an open output item".into()))?;
                        }
                        terminal = true;
                        if !reasoning_items.is_empty() {
                            yield ModelEvent::ProviderReplayState(
                                crate::model::ProviderReplayState {
                                    provider_kind: "openai_responses".into(),
                                    value: json!({"items": std::mem::take(&mut reasoning_items)}),
                                },
                            );
                        }
                        yield ModelEvent::Done(if saw_tool { FinishReason::ToolUse } else { FinishReason::Stop });
                    }
                    "response.incomplete" => {
                        if text_open || thinking_open || !tool_indices.is_empty() {
                            Err(Error::Provider("OpenAI Responses incomplete with an open output item".into()))?;
                        }
                        terminal = true;
                        yield ModelEvent::Done(FinishReason::Length);
                    }
                    "response.failed" => Err(Error::Provider(format!("OpenAI Responses failed: {}", event.data)))?,
                    _ => {}
                }
            }
            if !terminal {
                Err(Error::Provider("OpenAI Responses stream ended without response.completed or response.incomplete".into()))?;
            }
        })
    }
}

fn apply_model(
    body: &mut Value,
    model: &crate::model::ModelSpec,
    route_max_output_tokens: Option<u64>,
) -> Result<()> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| Error::Provider("OpenAI Responses request body is not an object".into()))?;
    if let Some(max) = model.max_output_tokens.or(route_max_output_tokens) {
        object.insert("max_output_tokens".into(), json!(max));
    }
    if model.reasoning.enabled || model.reasoning.effort.is_some() {
        let mut reasoning = Map::new();
        reasoning.insert("summary".into(), json!("auto"));
        if let Some(effort) = &model.reasoning.effort {
            reasoning.insert("effort".into(), json!(effort));
        }
        object.insert("reasoning".into(), Value::Object(reasoning));
    }
    if model.latency == ModelLatency::Fast {
        return Err(Error::Provider(
            "OpenAI Responses route cannot satisfy fast model latency".into(),
        ));
    }
    Ok(())
}

fn responses_input(messages: &[ProjectedMessage]) -> Result<Vec<Value>> {
    let mut input = Vec::new();
    for message in messages {
        match &message.content {
            ProjectedContent::Parts(parts) => {
                push_responses_parts(&mut input, &message.role, parts)?
            }
            ProjectedContent::ToolResult(result) => input.push(json!({
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": result.content,
            })),
            ProjectedContent::Assistant {
                text,
                replay_state,
                calls,
                ..
            } => {
                if let Some(state) = replay_state
                    .as_ref()
                    .filter(|state| state.provider_kind == "openai_responses")
                {
                    let items = state
                        .value
                        .get("items")
                        .and_then(Value::as_array)
                        .ok_or_else(|| {
                            Error::Protocol("OpenAI Responses replay state is missing items".into())
                        })?;
                    input.extend(items.iter().cloned());
                }
                push_responses_text(&mut input, &message.role, text);
                for call in calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.call_id,
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments)?,
                    }));
                }
            }
        }
    }
    Ok(input)
}

fn push_responses_parts(input: &mut Vec<Value>, role: &Role, parts: &[ContentPart]) -> Result<()> {
    let text_type = if *role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    let content = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } if text.is_empty() => None,
            ContentPart::Text { text } => Some(Ok(json!({"type":text_type, "text":text}))),
            ContentPart::Image { mime_type, data } if *role == Role::User => Some(Ok(json!({
                "type":"input_image",
                "image_url":format!("data:{mime_type};base64,{}", STANDARD.encode(data)),
            }))),
            ContentPart::Image { .. } => Some(Err(Error::Protocol(
                "OpenAI Responses only accepts images in user messages".into(),
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    if !content.is_empty() {
        input.push(json!({
            "type":"message",
            "role":role_name(role),
            "content":content,
        }));
    }
    Ok(())
}

fn push_responses_text(input: &mut Vec<Value>, role: &Role, text: &str) {
    if text.is_empty() {
        return;
    }
    let content_type = if *role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    input.push(json!({
        "type": "message",
        "role": role_name(role),
        "content": [{"type": content_type, "text": text}],
    }));
}

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event is missing {name}")))
}

fn required_u64(value: &Value, name: &str) -> Result<u64> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event is missing {name}")))
}

fn responses_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
        total_tokens: value.get("total_tokens").and_then(Value::as_u64),
        cache_read_tokens: value
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        cache_write_tokens: None,
        reasoning_tokens: value
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    }
}
