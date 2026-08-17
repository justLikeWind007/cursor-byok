use std::collections::HashMap;

use prost::Message;

use crate::{
    cursor::{prompting::fold_derived_state, proto::agent::v1 as pb},
    model::{CanonicalMessage, MessageContent},
    store::BlobId,
    Error, Result,
};

use super::CheckpointBuilder;

impl CheckpointBuilder {
    pub(super) async fn build_derived_state(
        &self,
        messages: &[CanonicalMessage],
    ) -> Result<(Vec<BlobId>, Option<BlobId>)> {
        let state = fold_derived_state(messages);
        let todo_values = state
            .todos
            .as_ref()
            .map(|value| {
                value
                    .get("todos")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| Error::Protocol("TodoWrite state is missing todos[]".into()))
            })
            .transpose()?;
        let mut todo_ids = Vec::new();
        for (index, todo) in todo_values.into_iter().flatten().enumerate() {
            let status = match todo
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Error::Protocol("TodoWrite item is missing status".into()))?
            {
                "in_progress" => pb::TodoStatus::InProgress,
                "completed" => pb::TodoStatus::Completed,
                "cancelled" => pb::TodoStatus::Cancelled,
                "pending" => pb::TodoStatus::Pending,
                status => {
                    return Err(Error::Protocol(format!(
                        "unknown TodoWrite status: {status}"
                    )))
                }
            };
            let message = pb::TodoItem {
                id: todo
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::Protocol("TodoWrite item is missing id".into()))?
                    .into(),
                content: todo
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::Protocol("TodoWrite item is missing content".into()))?
                    .into(),
                status: status as i32,
                created_at: 0,
                updated_at: 0,
                dependencies: todo
                    .get("dependencies")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect(),
            };
            let mut encoded = Vec::new();
            message.encode(&mut encoded)?;
            let id = BlobId::digest(&encoded);
            if self.base.todos.get(index).map(|raw| raw.as_slice()) == Some(id.as_bytes()) {
                todo_ids.push(id);
            } else {
                todo_ids.push(self.sync.persist(&encoded, &[]).await?);
            }
        }
        let plan_id = if let Some(value) = state.plan {
            let text = value
                .get("plan")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.as_str())
                .or_else(|| value.get("overview").and_then(serde_json::Value::as_str))
                .ok_or_else(|| Error::Protocol("plan state has no textual plan".into()))?;
            let mut encoded = Vec::new();
            pb::ConversationPlan { plan: text.into() }.encode(&mut encoded)?;
            let id = BlobId::digest(&encoded);
            if self.base.plan.as_deref() == Some(id.as_bytes()) {
                Some(id)
            } else {
                Some(self.sync.persist(&encoded, &[]).await?)
            }
        } else {
            None
        };
        Ok((todo_ids, plan_id))
    }
}

pub(super) fn update_current_step_state(
    messages: &[CanonicalMessage],
) -> Option<pb::CommunicateUpdateTurnState> {
    let result_indices = messages
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::ToolResult(result) => {
                update_message_index(&result.content).map(|index| (result.call_id.as_str(), index))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut state = pb::CommunicateUpdateTurnState::default();
    for message in messages {
        let MessageContent::Assistant { tool_calls, .. } = &message.content else {
            continue;
        };
        for call in tool_calls {
            if normalize(&call.name) != "updatecurrentstep" {
                continue;
            }
            if let (Some(step), Some(message_index)) = (
                call.arguments
                    .get("current_step")
                    .and_then(serde_json::Value::as_str),
                result_indices.get(call.call_id.as_str()),
            ) {
                state.history.push(pb::CommunicateUpdateHistoryEntry {
                    step: step.into(),
                    message_index: *message_index,
                });
            }
            if let Some(summary) = call
                .arguments
                .get("final_summary")
                .and_then(serde_json::Value::as_str)
            {
                state.final_summary = Some(summary.into());
            }
            if let Some(subtitle) = call
                .arguments
                .get("completed_subtitle")
                .and_then(serde_json::Value::as_str)
            {
                state.completed_subtitle = Some(subtitle.into());
            }
        }
    }
    (!state.history.is_empty()
        || state.final_summary.is_some()
        || state.completed_subtitle.is_some())
    .then_some(state)
}

fn update_message_index(output: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    value
        .get("success")
        .and_then(|success| success.get("message_index"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Origin, Role, ToolCallContent, ToolResultContent};

    #[test]
    fn update_current_step_is_folded_from_canonical_messages() {
        let messages = vec![
            CanonicalMessage {
                message_id: "assistant".into(),
                role: Role::Assistant,
                origin: Origin::Assistant,
                content: MessageContent::Assistant {
                    text: String::new(),
                    thinking: String::new(),
                    tool_round_id: Some("round".into()),
                    replay_state: None,
                    tool_calls: vec![ToolCallContent {
                        index: 0,
                        call_id: "call".into(),
                        name: "UpdateCurrentStep".into(),
                        arguments: serde_json::json!({
                            "current_step": "Inspecting protocol",
                            "final_summary": "Protocol verified.",
                            "completed_subtitle": "Verified protocol flow"
                        }),
                    }],
                },
                runtime_event_id: None,
            },
            CanonicalMessage {
                message_id: "result".into(),
                role: Role::Tool,
                origin: Origin::Tool,
                content: MessageContent::ToolResult(ToolResultContent {
                    call_id: "call".into(),
                    name: "UpdateCurrentStep".into(),
                    content: serde_json::json!({
                        "success": {"current_step": "Inspecting protocol", "message_index": 3}
                    })
                    .to_string(),
                    is_error: false,
                }),
                runtime_event_id: None,
            },
        ];
        let state = update_current_step_state(&messages).unwrap();
        assert_eq!(state.history[0].step, "Inspecting protocol");
        assert_eq!(state.history[0].message_index, 3);
        assert_eq!(state.final_summary.as_deref(), Some("Protocol verified."));
        assert_eq!(
            state.completed_subtitle.as_deref(),
            Some("Verified protocol flow")
        );
    }
}
