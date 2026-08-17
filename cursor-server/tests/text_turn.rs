#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use cursor_server::{
    cursor::prompting::{PromptAssets, PromptCompiler},
    cursor::{connect, proto::agent::v1 as pb},
    cursor::{CursorCommand, CursorSessionRegistry},
    model::{ProjectedContent, Role, Usage},
    provider::{FinishReason, ModelEvent},
};
use prost::Message;

#[tokio::test]
async fn text_turn_runs_from_bidi_request_through_checkpoint_and_end_stream() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "ignored".into(),
        },
        ModelEvent::ThinkingStart,
        ModelEvent::ThinkingDelta("reason".into()),
        ModelEvent::ThinkingEnd,
        ModelEvent::TextStart,
        ModelEvent::TextDelta("hello".into()),
        ModelEvent::TextEnd,
        ModelEvent::Usage(Usage {
            input_tokens: Some(20_000),
            output_tokens: Some(8),
            total_tokens: Some(20_008),
            cache_read_tokens: Some(80),
            cache_write_tokens: None,
            reasoning_tokens: Some(3),
        }),
        ModelEvent::Done(FinishReason::Stop),
    ]);
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
    let handle = registry.get_or_create("request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(client_run("conversation", "hello")),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut thinking_duration_ms = None;
    let mut saw_turn_ended = false;
    let mut token_deltas = Vec::new();
    let mut checkpoints = 0;
    let mut after_turn_checkpoints = Vec::new();
    let mut blobs = HashMap::<Vec<u8>, Vec<u8>>::new();
    let mut set_blob_ids = HashSet::new();
    let mut withheld_final_ack = false;
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
                    assert!(
                        set_blob_ids.insert(set.blob_id.clone()),
                        "an acknowledged content-addressed Blob must not be SET twice"
                    );
                    blobs.insert(set.blob_id.clone(), set.blob_data.clone());
                }
                if text == "hello" && !withheld_final_ack {
                    withheld_final_ack = true;
                    assert!(
                        tokio::time::timeout(std::time::Duration::from_millis(100), output.recv())
                            .await
                            .is_err(),
                        "TurnEnded/checkpoint must wait for the final Blob ACK"
                    );
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
            Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                match update.message {
                    Some(pb::interaction_update::Message::TextDelta(delta)) => {
                        text.push_str(&delta.text)
                    }
                    Some(pb::interaction_update::Message::ThinkingDelta(delta)) => {
                        assert_eq!(
                            delta.thinking_style,
                            Some(pb::ThinkingStyle::Default as i32)
                        );
                        thinking.push_str(&delta.text);
                    }
                    Some(pb::interaction_update::Message::ThinkingCompleted(completed)) => {
                        thinking_duration_ms = Some(completed.thinking_duration_ms)
                    }
                    Some(pb::interaction_update::Message::TurnEnded(usage)) => {
                        assert_eq!(usage.input_tokens, Some(20_000));
                        assert_eq!(usage.output_tokens, Some(8));
                        assert_eq!(usage.cache_read_tokens, Some(80));
                        assert_eq!(usage.reasoning_tokens, Some(3));
                        saw_turn_ended = true;
                    }
                    Some(pb::interaction_update::Message::TokenDelta(delta)) => {
                        token_deltas.push(delta.tokens)
                    }
                    _ => {}
                }
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                checkpoints += 1;
                if saw_turn_ended {
                    after_turn_checkpoints.push(state);
                }
            }
            _ => {}
        }
    }
    assert_eq!(text, "hello");
    assert_eq!(thinking, "reason");
    assert!(thinking_duration_ms.is_some_and(|duration| duration >= 1));
    assert!(saw_turn_ended);
    assert_eq!(token_deltas, vec![8]);
    assert!(withheld_final_ack);
    assert!(
        checkpoints >= 2,
        "final checkpoint is intentionally repeated"
    );
    assert_eq!(after_turn_checkpoints.len(), 3);
    assert_eq!(after_turn_checkpoints[0].pending_tool_calls.len(), 1);
    assert!(after_turn_checkpoints[1].pending_tool_calls.is_empty());
    assert_eq!(after_turn_checkpoints[1], after_turn_checkpoints[2]);
    let token_details = after_turn_checkpoints[1].token_details.as_ref().unwrap();
    assert_eq!(token_details.used_tokens, 20_008);
    assert_eq!(token_details.max_tokens, 256_000);
    let breakdown = token_details.breakdown.as_ref().unwrap();
    assert_eq!(breakdown.total_used_tokens, 20_008);
    assert_eq!(breakdown.max_tokens, 256_000);
    assert_eq!(breakdown.categories.len(), 8);
    assert_eq!(
        breakdown
            .categories
            .iter()
            .map(|category| category.estimated_tokens)
            .sum::<u32>(),
        20_008
    );
    assert_eq!(
        after_turn_checkpoints[0].turns, after_turn_checkpoints[1].turns,
        "staged and settled checkpoints reuse the same frozen Turn"
    );
    assert_eq!(
        after_turn_checkpoints[0].root_prompt_messages_json.len() + 1,
        after_turn_checkpoints[1].root_prompt_messages_json.len()
    );
    let pending: serde_json::Value =
        serde_json::from_str(&after_turn_checkpoints[0].pending_tool_calls[0]).unwrap();
    assert_eq!(pending["role"], "assistant");
    assert!(pending["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|part| part["type"] == "text" && part["text"] == "hello"));
    let final_root = after_turn_checkpoints[1]
        .root_prompt_messages_json
        .last()
        .unwrap();
    let stable: serde_json::Value = serde_json::from_slice(blobs.get(final_root).unwrap()).unwrap();
    assert_eq!(stable["role"], "assistant");
    assert!(
        stable.get("origin").is_none(),
        "wire root is not CanonicalMessage JSON"
    );
    let turn = pb::ConversationTurnStructure::decode(
        blobs
            .get(after_turn_checkpoints[0].turns.last().unwrap())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let pb::conversation_turn_structure::Turn::AgentConversationTurn(turn) = turn.turn.unwrap()
    else {
        panic!("expected agent turn")
    };
    assert_eq!(turn.steps.len(), 2, "thinking and text are frozen once");
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .prompt
        .instructions
        .contains("powered by test-model"));
    let projected = &requests[0].history;
    assert_eq!(projected[0].role, Role::User);
    let ProjectedContent::Parts(runtime) = &projected[0].content else {
        panic!("runtime context must be text")
    };
    assert!(matches!(
        runtime.as_slice(),
        [cursor_server::model::ContentPart::Text { text }]
            if text.contains("<user_info>")
                && text.contains("<user_query>\nhello\n</user_query>")
    ));
    assert_eq!(
        projected.len(),
        1,
        "the raw UserMessage is not projected twice"
    );

    let messages = store
        .load_current_messages(&cursor_server::model::ConversationId::new("conversation"))
        .await
        .unwrap();
    assert_eq!(messages[0].message_id, "runtime:run-request:request");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages.len(), 2, "runtime user plus final assistant");
    let stored_runs: Vec<String> = sqlx::query_scalar("SELECT run_id FROM runs ORDER BY run_id")
        .fetch_all(store.pool())
        .await
        .unwrap();
    assert_eq!(
        stored_runs,
        vec!["request"],
        "the concrete request_id, not Cursor's reusable wire run_id, owns the execution"
    );
}

fn client_run(conversation_id: &str, text: &str) -> pb::AgentClientMessage {
    let user = pb::UserMessage {
        text: text.into(),
        message_id: "user".into(),
        mode: pb::AgentMode::Agent as i32,
        ..Default::default()
    };
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(user),
                            request_context: Some(pb::RequestContext {
                                env: Some(pb::RequestContextEnv {
                                    os_version: "darwin".into(),
                                    workspace_paths: vec!["/workspace".into()],
                                    shell: "zsh".into(),
                                    terminals_folder: "/terminals".into(),
                                    agent_transcripts_folder: "/transcripts".into(),
                                    ..Default::default()
                                }),
                                git_repos: vec![pb::GitRepoInfo {
                                    path: "/workspace".into(),
                                    status: "M src/main.rs".into(),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some(conversation_id.into()),
                // Cursor provides the effective context limit even for a fresh conversation.
                conversation_state: Some(pb::ConversationStateStructure {
                    token_details: Some(pb::ConversationTokenDetails {
                        max_tokens: 256_000,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                run_id: Some("reusable-wire-run-id".into()),
                ..Default::default()
            },
        )),
    }
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
