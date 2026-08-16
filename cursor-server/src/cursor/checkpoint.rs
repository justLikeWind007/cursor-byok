use prost::Message;

use std::collections::HashSet;

use crate::{
    cursor::{blob_sync::BlobSynchronizer, interaction::render_tool_call, proto::agent::v1 as pb},
    model::{CanonicalMessage, MessageContent, Origin, Role, ToolCall},
    prompting::fold_derived_state,
    run::RunHandle,
    store::{BlobEdge, BlobId, Store},
    Error, Result,
};

pub struct CheckpointBuilder {
    store: Store,
    sync: BlobSynchronizer,
}

impl CheckpointBuilder {
    pub fn new(store: Store, sync: BlobSynchronizer) -> Self {
        Self { store, sync }
    }

    pub async fn build(
        &self,
        conversation_id: &str,
        revision: i64,
        messages: &[CanonicalMessage],
        mode: i32,
    ) -> Result<pb::ConversationStateStructure> {
        self.build_with_tool_progress(
            conversation_id,
            revision,
            messages,
            mode,
            &[],
            &HashSet::new(),
        )
        .await
    }

    pub async fn build_with_tool_progress(
        &self,
        conversation_id: &str,
        revision: i64,
        messages: &[CanonicalMessage],
        mode: i32,
        active_calls: &[ToolCall],
        completed: &HashSet<String>,
    ) -> Result<pb::ConversationStateStructure> {
        let mut root_ids = Vec::with_capacity(messages.len());
        for message in messages {
            root_ids.push(
                self.sync
                    .persist(&serde_json::to_vec(message)?, &[])
                    .await?,
            );
        }
        let turn_ids = self.build_turns(messages, mode, completed).await?;
        let (todo_ids, plan_id) = self.build_derived_state(messages).await?;
        let checkpoint = pb::ConversationStateStructure {
            root_prompt_messages_json: root_ids.iter().map(|id| id.as_bytes().to_vec()).collect(),
            turns: turn_ids.iter().map(|id| id.as_bytes().to_vec()).collect(),
            todos: todo_ids.iter().map(|id| id.as_bytes().to_vec()).collect(),
            plan: plan_id.as_ref().map(|id| id.as_bytes().to_vec()),
            pending_tool_calls: active_calls
                .iter()
                .filter(|call| !completed.contains(&call.call_id))
                .map(|call| call.call_id.clone())
                .collect(),
            mode: Some(mode),
            ..Default::default()
        };
        let mut encoded = Vec::new();
        checkpoint.encode(&mut encoded)?;
        let mut edges = root_ids
            .iter()
            .enumerate()
            .map(|(index, child)| BlobEdge {
                child: child.clone(),
                field_name: format!("root_prompt_messages_json[{index}]"),
            })
            .collect::<Vec<_>>();
        edges.extend(turn_ids.iter().enumerate().map(|(index, child)| BlobEdge {
            child: child.clone(),
            field_name: format!("turns[{index}]"),
        }));
        edges.extend(todo_ids.iter().enumerate().map(|(index, child)| BlobEdge {
            child: child.clone(),
            field_name: format!("todos[{index}]"),
        }));
        if let Some(child) = plan_id {
            edges.push(BlobEdge {
                child,
                field_name: "plan".into(),
            });
        }
        let head = self.sync.persist(&encoded, &edges).await?;
        if !self
            .store
            .publish_head(conversation_id, revision, &head)
            .await?
        {
            return Err(Error::Cancelled);
        }
        let dependencies = self.store.blob_closure(std::slice::from_ref(&head)).await?;
        self.store
            .enqueue_outbox(
                self.sync.request_id(),
                &format!("checkpoint:{}", head.to_base64()),
                "checkpoint",
                &encoded,
                &dependencies,
            )
            .await?;
        Ok(checkpoint)
    }

    pub async fn publish(
        &self,
        handle: &RunHandle,
        checkpoint: &pb::ConversationStateStructure,
    ) -> Result<()> {
        let encoded = checkpoint.encode_to_vec();
        let head = BlobId::digest(&encoded);
        let dependencies = self.store.blob_closure(std::slice::from_ref(&head)).await?;
        if !self
            .store
            .dependencies_acked(self.sync.request_id(), &dependencies)
            .await?
        {
            return Err(Error::Protocol(format!(
                "checkpoint {} published before Blob ACK barrier",
                head.to_base64()
            )));
        }
        handle.emit(&pb::AgentServerMessage {
            ttft_breakdown: None,
            message: Some(
                pb::agent_server_message::Message::ConversationCheckpointUpdate(checkpoint.clone()),
            ),
        })?;
        self.store
            .ack_outbox(
                self.sync.request_id(),
                &format!("checkpoint:{}", head.to_base64()),
            )
            .await?;
        Ok(())
    }

    async fn build_derived_state(
        &self,
        messages: &[CanonicalMessage],
    ) -> Result<(Vec<BlobId>, Option<BlobId>)> {
        let state = fold_derived_state(messages);
        let todo_values = state
            .todos
            .as_ref()
            .and_then(|value| value.get("todos").or(Some(value)))
            .and_then(serde_json::Value::as_array);
        let mut todo_ids = Vec::new();
        for todo in todo_values.into_iter().flatten() {
            let status = match todo
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("pending")
            {
                "in_progress" => pb::TodoStatus::InProgress,
                "completed" => pb::TodoStatus::Completed,
                "cancelled" => pb::TodoStatus::Cancelled,
                _ => pb::TodoStatus::Pending,
            };
            let message = pb::TodoItem {
                id: todo
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
                content: todo
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
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
            todo_ids.push(self.sync.persist(&encoded, &[]).await?);
        }
        let plan_id = if let Some(value) = state.plan {
            let text = value
                .get("plan")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.as_str())
                .unwrap_or_else(|| {
                    value
                        .get("overview")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                });
            let mut encoded = Vec::new();
            pb::ConversationPlan { plan: text.into() }.encode(&mut encoded)?;
            Some(self.sync.persist(&encoded, &[]).await?)
        } else {
            None
        };
        Ok((todo_ids, plan_id))
    }

    async fn build_turns(
        &self,
        messages: &[CanonicalMessage],
        mode: i32,
        completed_overlay: &HashSet<String>,
    ) -> Result<Vec<BlobId>> {
        let mut completed = completed_overlay.clone();
        for message in messages {
            if let MessageContent::ToolResult(result) = &message.content {
                completed.insert(result.call_id.clone());
            }
        }
        let mut turns = Vec::<(CanonicalMessage, Vec<&CanonicalMessage>)>::new();
        for message in messages {
            if message.role == Role::User && message.origin == Origin::User {
                turns.push((message.clone(), Vec::new()));
            } else if matches!(message.origin, Origin::Assistant | Origin::Tool) {
                if let Some((_, steps)) = turns.last_mut() {
                    steps.push(message);
                }
            }
        }
        let mut turn_ids = Vec::with_capacity(turns.len());
        for (user, step_messages) in turns {
            let text = match user.content {
                MessageContent::Text { text } => text,
                other => serde_json::to_string(&other)?,
            };
            let user_message = pb::UserMessage {
                text,
                message_id: user.message_id.clone(),
                mode,
                ..Default::default()
            };
            let mut encoded = Vec::new();
            user_message.encode(&mut encoded)?;
            let user_id = self.sync.persist(&encoded, &[]).await?;
            let mut step_ids = Vec::new();
            for message in step_messages {
                for step in message_steps(message, &completed)? {
                    let mut encoded = Vec::new();
                    step.encode(&mut encoded)?;
                    step_ids.push(self.sync.persist(&encoded, &[]).await?);
                }
            }
            let turn = pb::ConversationTurnStructure {
                turn: Some(
                    pb::conversation_turn_structure::Turn::AgentConversationTurn(
                        pb::AgentConversationTurnStructure {
                            user_message: user_id.as_bytes().to_vec(),
                            steps: step_ids.iter().map(|id| id.as_bytes().to_vec()).collect(),
                            request_id: None,
                            encrypted_model: None,
                            dynamic_tool_count: None,
                            send_message_step_indices: Vec::new(),
                        },
                    ),
                ),
            };
            let mut encoded = Vec::new();
            turn.encode(&mut encoded)?;
            let mut edges = vec![BlobEdge {
                child: user_id,
                field_name: "agent_conversation_turn.user_message".into(),
            }];
            edges.extend(
                step_ids
                    .into_iter()
                    .enumerate()
                    .map(|(index, child)| BlobEdge {
                        child,
                        field_name: format!("agent_conversation_turn.steps[{index}]"),
                    }),
            );
            turn_ids.push(self.sync.persist(&encoded, &edges).await?);
        }
        Ok(turn_ids)
    }

    pub async fn import_prefetched(&self, blobs: &[pb::PreFetchedBlob]) -> Result<()> {
        for blob in blobs {
            let expected = BlobId::from_bytes(&blob.id)?;
            let actual = self.store.put_blob(&blob.value, &[]).await?;
            if expected != actual {
                return Err(Error::Protocol(format!(
                    "prefetched Blob hash mismatch: {}",
                    expected.to_base64()
                )));
            }
        }
        Ok(())
    }

    pub async fn hydrate_messages(
        &self,
        state: Option<&pb::ConversationStateStructure>,
    ) -> Result<Vec<CanonicalMessage>> {
        let mut messages = Vec::new();
        let Some(state) = state else {
            return Ok(messages);
        };
        for raw_id in &state.root_prompt_messages_json {
            let id = BlobId::from_bytes(raw_id)?;
            let Some(data) = self.sync.get(&id).await? else {
                return Err(Error::Protocol(format!(
                    "missing message Blob {}",
                    id.to_base64()
                )));
            };
            messages.push(serde_json::from_slice(&data)?);
        }
        Ok(messages)
    }
}

fn message_steps(
    message: &CanonicalMessage,
    completed: &HashSet<String>,
) -> Result<Vec<pb::ConversationStep>> {
    use pb::conversation_step::Message;
    match &message.content {
        MessageContent::Assistant {
            text,
            thinking,
            tool_calls,
            ..
        } => {
            let mut steps = Vec::new();
            if !thinking.is_empty() {
                steps.push(pb::ConversationStep {
                    message: Some(Message::ThinkingMessage(pb::ThinkingMessage {
                        text: thinking.clone(),
                        duration_ms: 0,
                    })),
                });
            }
            if !text.is_empty() {
                steps.push(pb::ConversationStep {
                    message: Some(Message::AssistantMessage(pb::AssistantMessage {
                        text: text.clone(),
                    })),
                });
            }
            for call in tool_calls {
                let tool = render_tool_call(
                    &ToolCall {
                        index: 0,
                        call_id: call.call_id.clone(),
                        model_call_id: String::new(),
                        name: call.name.clone(),
                        arguments_text: serde_json::to_string(&call.arguments).unwrap_or_default(),
                        arguments: call.arguments.clone(),
                    },
                    completed.contains(&call.call_id),
                )?;
                steps.push(pb::ConversationStep {
                    message: Some(Message::ToolCall(tool)),
                });
            }
            Ok(steps)
        }
        _ => Ok(Vec::new()),
    }
}
