mod query;
mod render;

use std::time::Duration;

use crate::{
    cursor::proto::agent::v1 as pb,
    model::{ToolCall, Usage},
    provider::ModelEvent,
    Result,
};

pub use query::tool_query;
pub(crate) use render::{edit_content_delta, edit_path_partial};
pub use render::{render_tool_call, tool_completed, tool_placeholder, tool_started};

pub fn response_event(
    event: &ModelEvent,
    model_call_id: &str,
) -> Result<Option<pb::AgentServerMessage>> {
    use pb::interaction_update::Message;
    let message = match event {
        ModelEvent::TextDelta(text) => Message::TextDelta(pb::TextDeltaUpdate {
            text: text.clone(),
            is_server_notice: false,
        }),
        ModelEvent::ThinkingDelta(text) => Message::ThinkingDelta(pb::ThinkingDeltaUpdate {
            text: text.clone(),
            thinking_style: Some(pb::ThinkingStyle::Default as i32),
        }),
        ModelEvent::ToolCallStart { call_id, name, .. } => {
            Message::PartialToolCall(pb::PartialToolCallUpdate {
                call_id: call_id.clone(),
                tool_call: Some(tool_placeholder(name, call_id)?),
                args_text_delta: String::new(),
                model_call_id: model_call_id.into(),
            })
        }
        ModelEvent::ToolCallArgumentsDelta { .. } => return Ok(None),
        ModelEvent::ToolCallEnd { .. }
        | ModelEvent::Start { .. }
        | ModelEvent::TextStart
        | ModelEvent::TextEnd
        | ModelEvent::ThinkingStart
        | ModelEvent::ThinkingEnd
        | ModelEvent::ProviderReplayState(_)
        | ModelEvent::Usage(_)
        | ModelEvent::Done(_) => return Ok(None),
    };
    Ok(Some(server_interaction(message)))
}

pub fn thinking_completed(elapsed: Duration) -> pb::AgentServerMessage {
    let milliseconds = elapsed.as_millis().clamp(1, i32::MAX as u128) as i32;
    server_interaction(pb::interaction_update::Message::ThinkingCompleted(
        pb::ThinkingCompletedUpdate {
            thinking_duration_ms: milliseconds,
        },
    ))
}

pub fn arguments_delta(call: &ToolCall, delta: &str) -> Result<pb::AgentServerMessage> {
    Ok(server_interaction(
        pb::interaction_update::Message::PartialToolCall(pb::PartialToolCallUpdate {
            call_id: call.call_id.clone(),
            tool_call: Some(tool_placeholder(&call.name, &call.call_id)?),
            args_text_delta: delta.into(),
            model_call_id: call.model_call_id.clone(),
        }),
    ))
}

pub fn turn_ended(usage: Option<Usage>) -> pb::AgentServerMessage {
    server_interaction(pb::interaction_update::Message::TurnEnded(
        pb::TurnEndedUpdate {
            input_tokens: usage.and_then(|usage| usage.input_tokens.map(|value| value as i64)),
            output_tokens: usage.and_then(|usage| usage.output_tokens.map(|value| value as i64)),
            cache_read_tokens: usage
                .and_then(|usage| usage.cache_read_tokens.map(|value| value as i64)),
            cache_write_tokens: usage
                .and_then(|usage| usage.cache_write_tokens.map(|value| value as i64)),
            reasoning_tokens: usage
                .and_then(|usage| usage.reasoning_tokens.map(|value| value as i64)),
        },
    ))
}

pub fn token_delta(tokens: u64) -> pb::AgentServerMessage {
    server_interaction(pb::interaction_update::Message::TokenDelta(
        pb::TokenDeltaUpdate {
            tokens: tokens.min(i32::MAX as u64) as i32,
        },
    ))
}

pub fn server_interaction(message: pb::interaction_update::Message) -> pb::AgentServerMessage {
    pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::InteractionUpdate(
            pb::InteractionUpdate {
                message: Some(message),
            },
        )),
    }
}
