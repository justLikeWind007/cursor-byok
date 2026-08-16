#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use cursor_server::{
    cursor::{
        connect,
        proto::{agent::v1 as pb, aiserver::v1 as ai},
    },
    model::{MessageContent, Role},
    prompting::{PromptAssets, PromptCompiler},
    provider::{FinishReason, ResponseEvent},
    run::{RunCommand, RunRegistry},
    Error,
};
use prost::Message;

#[tokio::test]
async fn provider_failure_checkpoints_then_returns_structured_error_and_closes() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push_error(Error::Provider("provider failed".into()));
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    let registry = RunRegistry::new(
        store.clone(),
        Arc::new(provider),
        PromptCompiler::new(assets),
        "test-model".into(),
    );
    let handle = registry.get_or_create("failed-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(RunCommand::Append {
            seqno: 0,
            message: Box::new(client_run()),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let mut saw_checkpoint = false;
    let mut saw_turn_ended = false;
    let error_json = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .expect("RunSSE closed before EndStream");
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break serde_json::from_slice::<serde_json::Value>(&payload).unwrap();
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                handle
                    .command(RunCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(_)) => {
                saw_checkpoint = true;
            }
            Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                if matches!(
                    update.message,
                    Some(pb::interaction_update::Message::TurnEnded(_))
                ) {
                    saw_turn_ended = true;
                }
                if let Some(pb::interaction_update::Message::TextDelta(delta)) = update.message {
                    assert!(!delta.text.contains("Cursor server error"));
                }
            }
            _ => {}
        }
    };

    assert!(saw_checkpoint);
    assert!(!saw_turn_ended);
    assert_eq!(error_json["error"]["code"], "unavailable");
    let detail = &error_json["error"]["details"][0];
    assert_eq!(detail["type"], "aiserver.v1.ErrorDetails");
    let encoded = detail["value"].as_str().unwrap();
    assert!(!encoded.ends_with('='));
    let decoded = STANDARD_NO_PAD.decode(encoded).unwrap();
    let decoded = ai::ErrorDetails::decode(decoded.as_slice()).unwrap();
    assert_eq!(
        decoded.error,
        ai::error_details::Error::ProviderError as i32
    );
    let custom = decoded.details.unwrap();
    assert_eq!(custom.title, "Server Error");
    assert_eq!(custom.is_retryable, Some(true));
    assert_eq!(custom.should_show_immediate_error, Some(false));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), output.recv())
            .await
            .unwrap(),
        None
    );

    let messages = store.load_messages("failed-conversation").await.unwrap();
    assert!(messages.iter().any(|message| message.role == Role::User));
    assert!(!messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::Assistant { text, .. } if text.contains("Cursor server error")
        )
    }));
}

#[tokio::test]
async fn runtime_protocol_failure_returns_connect_error_end_stream_and_closes() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ResponseEvent::Start {
            model_call_id: "model-call".into(),
        },
        ResponseEvent::ToolCallStart {
            index: 0,
            call_id: "call-1".into(),
            name: "Read".into(),
        },
        ResponseEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: "{\"path\":\"/tmp/a\"}".into(),
        },
        ResponseEvent::ToolCallEnd { index: 0 },
        ResponseEvent::Done(FinishReason::ToolUse),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    let registry = RunRegistry::new(
        store,
        Arc::new(provider),
        PromptCompiler::new(assets),
        "test-model".into(),
    );
    let handle = registry
        .get_or_create("protocol-failed-request")
        .await
        .unwrap();
    let mut output = handle.subscribe();
    handle
        .command(RunCommand::Append {
            seqno: 0,
            message: Box::new(protocol_client_run()),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let mut saw_turn_ended = false;
    let error_json = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .expect("RunSSE closed before Error EndStream");
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break serde_json::from_slice::<serde_json::Value>(&payload).unwrap();
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                handle
                    .command(RunCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                // An unknown numeric bridge id is a runtime protocol error.
                handle
                    .command(RunCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(pb::AgentClientMessage {
                            message: Some(pb::agent_client_message::Message::ExecClientMessage(
                                pb::ExecClientMessage {
                                    id: exec.id + 1_000,
                                    exec_id: String::new(),
                                    message: None,
                                    ..Default::default()
                                },
                            )),
                        }),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                if matches!(
                    update.message,
                    Some(pb::interaction_update::Message::TurnEnded(_))
                ) {
                    saw_turn_ended = true;
                }
                if let Some(pb::interaction_update::Message::TextDelta(delta)) = update.message {
                    assert!(!delta.text.contains("unknown tool result"));
                    assert!(!delta.text.contains("protocol error"));
                }
            }
            _ => {}
        }
    };

    assert!(!saw_turn_ended);
    assert_eq!(error_json["error"]["code"], "invalid_argument");
    assert_eq!(
        error_json["error"]["message"],
        "protocol error: unknown ExecClientMessage id: 1001"
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), output.recv())
            .await
            .unwrap(),
        None
    );
}

fn client_run() -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "hello".into(),
                                message_id: "failed-user".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("failed-conversation".into()),
                run_id: Some("failed-request".into()),
                ..Default::default()
            },
        )),
    }
}

fn protocol_client_run() -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "read it".into(),
                                message_id: "protocol-failed-user".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("protocol-failed-conversation".into()),
                run_id: Some("protocol-failed-request".into()),
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
