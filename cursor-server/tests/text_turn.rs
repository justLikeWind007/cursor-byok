#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use cursor_server::{
    cursor::{connect, proto::agent::v1 as pb},
    prompting::{PromptAssets, PromptCompiler},
    provider::{FinishReason, ResponseEvent},
    run::{RunCommand, RunRegistry},
};
use prost::Message;

#[tokio::test]
async fn text_turn_runs_from_bidi_request_through_checkpoint_and_end_stream() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ResponseEvent::Start {
            model_call_id: "ignored".into(),
        },
        ResponseEvent::ThinkingStart,
        ResponseEvent::ThinkingDelta("reason".into()),
        ResponseEvent::ThinkingEnd,
        ResponseEvent::TextStart,
        ResponseEvent::TextDelta("hello".into()),
        ResponseEvent::TextEnd,
        ResponseEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    let registry = RunRegistry::new(
        store.clone(),
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        "test-model".into(),
    );
    let handle = registry.get_or_create("request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(RunCommand::Append {
            seqno: 0,
            message: Box::new(client_run("request", "conversation", "hello")),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut thinking_duration_ms = None;
    let mut saw_turn_ended = false;
    let mut checkpoints = 0;
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
                handle
                    .command(RunCommand::Append {
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
                    Some(pb::interaction_update::Message::TurnEnded(_)) => saw_turn_ended = true,
                    _ => {}
                }
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(_)) => {
                checkpoints += 1
            }
            _ => {}
        }
    }
    assert_eq!(text, "hello");
    assert_eq!(thinking, "reason");
    assert!(thinking_duration_ms.is_some_and(|duration| duration >= 1));
    assert!(saw_turn_ended);
    assert_eq!(checkpoints, 2, "final checkpoint is intentionally repeated");
    assert_eq!(provider.requests().len(), 1);
    assert!(store
        .load_messages("conversation")
        .await
        .unwrap()
        .iter()
        .any(|message| message.message_id == "user"));
}

fn client_run(request_id: &str, conversation_id: &str, text: &str) -> pb::AgentClientMessage {
    let user = pb::UserMessage {
        text: text.into(),
        message_id: "user".into(),
        mode: pb::AgentMode::Agent as i32,
        ..Default::default()
    };
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(user),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some(conversation_id.into()),
                run_id: Some(request_id.into()),
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
