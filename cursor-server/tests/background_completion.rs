#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{collections::HashMap, sync::Arc};

use cursor_server::{
    cursor::{
        connect,
        prompting::{PromptAssets, PromptCompiler},
        proto::agent::v1 as pb,
        CursorCommand, CursorSessionHandle, CursorSessionRegistry,
    },
    model::{ContentPart, MessageContent, ProjectedContent, Role},
    provider::{FinishReason, ModelEvent},
};
use prost::Message;

const FOLLOW_UP: &str = "Perform any necessary follow-up actions in response to the subagent completion above. If no follow-up work is needed, no further action is required. If you mention an agent or subagent in your response, link it with the `[Name](id)` Don't use generic label such as `[agent]`, `[worker]`, or `[subagent]`.";

#[tokio::test]
async fn background_subagent_completion_starts_a_simulated_parent_turn() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(stop_response("model-call", "followed up"));
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store.clone(),
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        Default::default(),
    );
    let handle = registry.get_or_create("completion-request").await.unwrap();
    let (checkpoint, blobs) = drive_completion(
        &handle,
        completion_run(
            "child-id",
            "reusable-parent-run",
            pb::ConversationStateStructure {
                mode: Some(pb::AgentMode::Multitask as i32),
                ..Default::default()
            },
        ),
    )
    .await;

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let [runtime] = requests[0].history.as_slice() else {
        panic!("completion Run must add exactly one runtime message")
    };
    assert_eq!(runtime.role, Role::User);
    let ProjectedContent::Parts(parts) = &runtime.content else {
        panic!("completion context must be text")
    };
    let [ContentPart::Text { text }] = parts.as_slice() else {
        panic!("completion context must have one text part")
    };
    assert!(text.contains("Subagent ID: child-id"));
    assert!(text.contains("child result"));
    assert!(text.contains(FOLLOW_UP));

    let messages = store
        .load_current_messages(&cursor_server::model::ConversationId::new(
            "parent-conversation",
        ))
        .await
        .unwrap();
    assert!(messages.iter().any(|message| {
        message.runtime_event_id.as_deref() == Some("run-request:completion-request")
            && matches!(&message.content, MessageContent::Parts { parts } if !parts.is_empty())
    }));

    let turn = pb::ConversationTurnStructure::decode(
        blobs
            .get(checkpoint.turns.last().expect("completion Turn"))
            .expect("completion Turn Blob")
            .as_slice(),
    )
    .unwrap();
    let pb::conversation_turn_structure::Turn::AgentConversationTurn(turn) = turn.turn.unwrap()
    else {
        panic!("expected agent conversation Turn")
    };
    let user = pb::UserMessage::decode(
        blobs
            .get(&turn.user_message)
            .expect("simulated UserMessage Blob")
            .as_slice(),
    )
    .unwrap();
    assert!(user.text.contains(FOLLOW_UP));
    assert_eq!(user.is_simulated_msg, Some(true));
    assert_eq!(
        user.simulated_msg_reason,
        Some(pb::SimulatedMsgReason::BackgroundTaskCompletion as i32)
    );
    assert_eq!(
        user.simulated_message_metadata.unwrap().task_id.as_deref(),
        Some("child-id")
    );

    provider.push(stop_response("model-call-2", "followed up again"));
    let second = registry
        .get_or_create("completion-request-2")
        .await
        .unwrap();
    drive_completion(
        &second,
        completion_run("child-id-2", "reusable-parent-run-2", checkpoint),
    )
    .await;

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let runtime_ids = requests[1]
        .history
        .iter()
        .map(|message| message.message_id.as_str())
        .filter(|id| id.starts_with("runtime:"))
        .collect::<Vec<_>>();
    assert_eq!(
        runtime_ids,
        [
            "runtime:run-request:completion-request",
            "runtime:run-request:completion-request-2"
        ]
    );
}

async fn drive_completion(
    handle: &CursorSessionHandle,
    message: pb::AgentClientMessage,
) -> (pb::ConversationStateStructure, HashMap<Vec<u8>, Vec<u8>>) {
    let mut output = handle.subscribe();
    handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(message),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let mut blobs = HashMap::new();
    let mut final_checkpoint = None;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                if let Some(pb::kv_server_message::Message::SetBlobArgs(set)) = &kv.message {
                    blobs.insert(set.blob_id.clone(), set.blob_data.clone());
                }
                handle
                    .command(CursorCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state))
                if state.pending_tool_calls.is_empty() =>
            {
                final_checkpoint = Some(state);
            }
            _ => {}
        }
    }
    (
        final_checkpoint.expect("settled completion checkpoint"),
        blobs,
    )
}

fn completion_run(
    child_id: &str,
    run_id: &str,
    conversation_state: pb::ConversationStateStructure,
) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(
                        pb::conversation_action::Action::BackgroundTaskCompletionAction(
                            pb::BackgroundTaskCompletionAction {
                                completions: vec![pb::BackgroundTaskCompletion {
                                    task_id: child_id.into(),
                                    kind: pb::BackgroundTaskKind::Subagent as i32,
                                    status: pb::BackgroundTaskStatus::Success as i32,
                                    title: "Inspect protocol".into(),
                                    detail: Some("child result".into()),
                                    output_path: Some("/tmp/child.jsonl".into()),
                                    reason: pb::BackgroundTaskCompletionReason::TaskFinished as i32,
                                    subagent_id: Some(child_id.into()),
                                    tool_call_id: Some("task-call".into()),
                                    ..Default::default()
                                }],
                            },
                        ),
                    ),
                    ..Default::default()
                }),
                conversation_id: Some("parent-conversation".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                conversation_state: Some(conversation_state),
                run_id: Some(run_id.into()),
                ..Default::default()
            },
        )),
    }
}

fn stop_response(model_call_id: &str, text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Start {
            model_call_id: model_call_id.into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta(text.into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]
}

fn kv_ack(id: u32) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::KvClientMessage(
            pb::KvClientMessage {
                id,
                message: Some(pb::kv_client_message::Message::SetBlobResult(
                    pb::SetBlobResult { error: None },
                )),
            },
        )),
    }
}
