#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use cursor_server::{
    cursor::prompting::{PromptAssets, PromptCompiler},
    cursor::{connect, proto::agent::v1 as pb},
    cursor::{CursorCommand, CursorSessionRegistry},
    model::{ConversationId, ModelSpec, PreparedRun, PromptSpec, RunAction, RunId, RunKind},
    provider::{FinishReason, ModelEvent},
    run::RunRegistry,
    store::RunStatus,
};
use prost::Message;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn generic_run_registry_cancels_the_previous_client_for_a_conversation() {
    let registry = RunRegistry::default();
    let conversation = cursor_server::model::ConversationId::new("conversation");
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    registry
        .activate(
            conversation.clone(),
            cursor_server::model::RunId::new("first"),
            first.clone(),
        )
        .await;
    registry
        .activate(
            conversation.clone(),
            cursor_server::model::RunId::new("second"),
            second.clone(),
        )
        .await;

    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    registry
        .release(&conversation, &cursor_server::model::RunId::new("first"))
        .await;
    registry.shutdown().await;
    assert!(second.is_cancelled());
}

#[tokio::test]
async fn a_replaced_run_cannot_overwrite_its_cancelled_status() {
    let (_directory, store) = fixtures::temp_store().await;
    let conversation_id = ConversationId::new("conversation");
    let base_revision_id = store.ensure_conversation(&conversation_id).await.unwrap();
    let prepared = |run_id: &str| PreparedRun {
        run_id: RunId::new(run_id),
        conversation_id: conversation_id.clone(),
        kind: RunKind::Root,
        model: ModelSpec::new("model"),
        prompt: PromptSpec {
            instructions: String::new(),
            tools: Vec::new(),
        },
        selected_subagent_models: Vec::new(),
        subagent_model_overrides: Vec::new(),
        initial_messages: Vec::new(),
        action: RunAction::Resume {
            pending_tool_round: None,
        },
        base_revision_id,
    };
    let first = prepared("first");
    let second = prepared("second");

    store.claim_run(&first).await.unwrap();
    store.claim_run(&second).await.unwrap();
    assert!(!store
        .finish_run(&first.run_id, RunStatus::Completed, None, None,)
        .await
        .unwrap());

    let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE run_id = 'first'")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let active: Option<String> = sqlx::query_scalar(
        "SELECT active_run_id FROM conversations WHERE conversation_id = 'conversation'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(status, "cancelled");
    assert_eq!(active.as_deref(), Some("second"));
}

#[tokio::test]
async fn registry_shutdown_cancels_runs_and_closes_run_sse_outputs() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store,
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
        Default::default(),
    );
    let handle = registry.get_or_create("active-run").await.unwrap();
    let mut output = handle.subscribe();

    registry.shutdown().await;

    assert!(handle.cancellation().is_cancelled());
    let terminal = output.recv().await.expect("canceled EndStream");
    let (flags, payload) = connect::decode_frames(&terminal).unwrap().pop().unwrap();
    assert_eq!(flags, connect::END_STREAM_FLAG);
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload["error"]["code"], "canceled");
    assert_eq!(output.recv().await, None);
}

#[tokio::test]
async fn cancel_aborts_active_exec_before_canceled_end_stream() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "ignored".into(),
        },
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: "call-1".into(),
            name: "Read".into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: "{\"path\":\"/tmp/a\"}".into(),
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store,
        Arc::new(provider),
        PromptCompiler::new(assets),
        Default::default(),
    );
    let handle = registry.get_or_create("cancel-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(client_run()),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    let exec_id = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        assert_eq!(
            flags & connect::END_STREAM_FLAG,
            0,
            "Run ended before Exec: {}",
            String::from_utf8_lossy(&payload)
        );
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                handle
                    .command(CursorCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => break exec.id,
            _ => {}
        }
    };

    handle.cancel();
    let mut saw_abort = false;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .expect("RunSSE closed before canceled EndStream");
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(json["error"]["code"], "canceled");
            assert!(saw_abort, "ExecServerAbort must precede canceled EndStream");
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        if let Some(pb::agent_server_message::Message::ExecServerControlMessage(control)) =
            server.message
        {
            let Some(pb::exec_server_control_message::Message::Abort(abort)) = control.message
            else {
                panic!("expected ExecServerAbort")
            };
            assert_eq!(abort.id, exec_id);
            saw_abort = true;
        }
    }
    assert_eq!(output.recv().await, None);
}

fn client_run() -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "read".into(),
                                message_id: "cancel-user".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("cancel-conversation".into()),
                run_id: Some("cancel-request".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
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
