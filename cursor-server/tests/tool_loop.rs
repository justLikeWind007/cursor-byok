#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use cursor_server::{
    cursor::{
        connect, exec,
        pending::{ExecContext, PendingExecRegistry},
        proto::agent::v1 as pb,
    },
    model::{MessageContent, ToolCall},
    prompting::{PromptAssets, PromptCompiler},
    provider::{FinishReason, ResponseEvent},
    run::{RunCommand, RunRegistry},
};
use prost::Message;
use serde_json::json;

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        index: 0,
        call_id: id.into(),
        model_call_id: "model:0".into(),
        name: name.into(),
        arguments_text: "{}".into(),
        arguments: json!({}),
    }
}

fn exec_context() -> ExecContext {
    ExecContext {
        conversation_id: "conversation".into(),
        terminals_folder: "/tmp/terminals".into(),
        admin_command_denylist: Vec::new(),
    }
}

#[test]
fn dynamic_mcp_call_routes_to_the_captured_exec_message() {
    let call = ToolCall {
        index: 0,
        call_id: "mcp-call".into(),
        model_call_id: "model:0".into(),
        name: "mcp_repo_lookup".into(),
        arguments_text: "{\"query\":\"x\"}".into(),
        arguments: json!({"query": "x"}),
    };
    let definition = pb::McpToolDefinition {
        name: "mcp_repo_lookup".into(),
        provider_identifier: "repo".into(),
        tool_name: "lookup".into(),
        description: "lookup".into(),
        input_schema: None,
        input_schema_json: None,
    };
    let message = cursor_server::cursor::exec::mcp_request(7, &call, &definition).unwrap();
    let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) = message.message else {
        panic!("expected ExecServerMessage")
    };
    let Some(pb::exec_server_message::Message::McpArgs(args)) = exec.message else {
        panic!("expected McpArgs")
    };
    assert_eq!(exec.exec_id, "mcp-call");
    assert_eq!(args.provider_identifier, "repo");
    assert_eq!(args.tool_name, "lookup");
    assert_eq!(
        args.args["query"].kind,
        Some(prost_types::value::Kind::StringValue("x".into()))
    );
}

#[tokio::test]
async fn shell_uses_background_timeout_and_preserves_stream_identity() {
    let mut shell = call("call-shell", "Shell");
    shell.arguments = json!({
        "command": "python3 -m http.server 8000",
        "working_directory": "/tmp/project",
        "block_until_ms": 3000,
        "description": "Start HTTP server"
    });
    let context = exec_context();
    let request = exec::request(7, &shell, &context).unwrap();
    let Some(pb::agent_server_message::Message::ExecServerMessage(request)) = request.message
    else {
        panic!("expected ExecServerMessage")
    };
    assert_eq!(request.accept_hook_additional_contexts, Some(true));
    let Some(pb::exec_server_message::Message::ShellStreamArgs(args)) = request.message else {
        panic!("expected ShellArgs")
    };
    assert_eq!(args.timeout, 3000);
    assert_eq!(
        args.timeout_behavior,
        pb::TimeoutBehavior::Background as i32
    );
    assert_eq!(args.hard_timeout, Some(86_400_000));
    assert_eq!(args.description.as_deref(), Some("Start HTTP server"));
    assert!(args.close_stdin);
    assert_eq!(args.conversation_id.as_deref(), Some("conversation"));
    assert_eq!(args.file_output_threshold_bytes, Some(40_000));

    let pending = PendingExecRegistry::default();
    let id = pending.reserve(&shell, &context).await.unwrap();
    let delta = exec::client_event(
        &pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::ShellStream(
                pb::ShellStream {
                    event: Some(pb::shell_stream::Event::Stdout(pb::ShellStreamStdout {
                        data: "Serving HTTP on port 8000\n".into(),
                    })),
                },
            )),
            ..Default::default()
        },
        &pending,
    )
    .await
    .unwrap();
    let exec::ClientExecEvent::Delta(delta) = delta else {
        panic!("expected Shell stdout delta")
    };
    let Some(pb::agent_server_message::Message::InteractionUpdate(delta)) = delta.message else {
        panic!("expected InteractionUpdate")
    };
    let Some(pb::interaction_update::Message::ToolCallDelta(delta)) = delta.message else {
        panic!("expected ToolCallDelta")
    };
    assert_eq!(delta.call_id, "call-shell");
    assert_eq!(delta.model_call_id, "model:0");
    let Some(pb::tool_call_delta::Delta::ShellToolCallDelta(shell_delta)) =
        delta.tool_call_delta.and_then(|delta| delta.delta)
    else {
        panic!("expected ShellToolCallDelta")
    };
    let Some(pb::shell_tool_call_delta::Delta::Stdout(stdout)) = shell_delta.delta else {
        panic!("expected stdout")
    };
    assert_eq!(stdout.content, "Serving HTTP on port 8000\n");

    let completion = exec::client_event(
        &pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::ShellStream(
                pb::ShellStream {
                    event: Some(pb::shell_stream::Event::Backgrounded(
                        pb::ShellStreamBackgrounded {
                            shell_id: 42,
                            command: "python3 -m http.server 8000".into(),
                            working_directory: "/tmp/project".into(),
                            pid: Some(1234),
                            ms_to_wait: Some(3000),
                            reason: Some(pb::ShellBackgroundReason::Timeout as i32),
                        },
                    )),
                },
            )),
            ..Default::default()
        },
        &pending,
    )
    .await
    .unwrap();
    let exec::ClientExecEvent::Completed(completion) = completion else {
        panic!("expected background completion")
    };
    assert_eq!(
        completion.result().output.as_str(),
        Some(
            "shell running in background shell_id=42 pid=1234 terminals_folder=/tmp/terminals\nServing HTTP on port 8000\n"
        )
    );
    let Some(pb::tool_call::Tool::ShellToolCall(tool)) = &completion.tool_call().tool else {
        panic!("expected ShellToolCall")
    };
    let result = tool.result.as_ref().expect("background ShellResult");
    assert_eq!(result.is_background, Some(true));
    assert_eq!(result.terminals_folder.as_deref(), Some("/tmp/terminals"));
    assert_eq!(result.pid, Some(1234));
}

#[tokio::test]
async fn exec_ids_are_monotonic_and_released_ids_are_not_reused() {
    let pending = PendingExecRegistry::default();
    let first = pending
        .reserve(&call("call-1", "Read"), &exec_context())
        .await
        .unwrap();
    assert_eq!(first, 1);
    assert_eq!(
        pending.call(first).await.map(|call| call.call_id),
        Some("call-1".into())
    );
    pending.discard(first).await;
    assert!(pending.call(first).await.is_none());

    let second = pending
        .reserve(&call("call-2", "Read"), &exec_context())
        .await
        .unwrap();
    assert_eq!(second, 2, "released Exec ids must not be reused in one Run");
}

#[tokio::test]
async fn empty_exec_client_message_is_not_a_terminal_result() {
    let pending = PendingExecRegistry::default();
    let id = pending
        .reserve(&call("call-1", "Read"), &exec_context())
        .await
        .unwrap();
    let event = exec::client_event(
        &pb::ExecClientMessage {
            id,
            message: None,
            ..Default::default()
        },
        &pending,
    )
    .await
    .unwrap();
    assert!(matches!(event, exec::ClientExecEvent::Pending));
    assert_eq!(
        pending.call(id).await.map(|call| call.call_id),
        Some("call-1".into())
    );
}

#[tokio::test]
async fn tool_success_is_not_inferred_from_debug_text() {
    let pending = PendingExecRegistry::default();
    let mut write = call("call-1", "Write");
    write.arguments = json!({"path": "/tmp/a", "contents": "x"});
    let id = pending.reserve(&write, &exec_context()).await.unwrap();
    let event = exec::client_event(
        &pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::WriteResult(
                pb::WriteResult {
                    result: Some(pb::write_result::Result::Success(pb::WriteSuccess {
                        path: "/tmp/a".into(),
                        file_content_after_write: Some("enum Error { Example }".into()),
                        ..Default::default()
                    })),
                },
            )),
            ..Default::default()
        },
        &pending,
    )
    .await
    .unwrap();
    let exec::ClientExecEvent::Completed(completion) = event else {
        panic!("expected terminal write result")
    };
    assert!(!completion.result().is_error);
    assert!(matches!(
        completion.tool_call().tool,
        Some(pb::tool_call::Tool::EditToolCall(_))
    ));
}

#[tokio::test]
async fn an_exec_result_must_match_the_reserved_tool() {
    let pending = PendingExecRegistry::default();
    let id = pending
        .reserve(&call("call-1", "Read"), &exec_context())
        .await
        .unwrap();
    let result = exec::client_event(
        &pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::WriteResult(
                pb::WriteResult {
                    result: Some(pb::write_result::Result::Success(pb::WriteSuccess {
                        path: "/tmp/a".into(),
                        ..Default::default()
                    })),
                },
            )),
            ..Default::default()
        },
        &pending,
    )
    .await;
    let Err(error) = result else {
        panic!("mismatched result must fail")
    };
    assert!(error
        .to_string()
        .contains("unexpected Exec result for tool Read"));
    assert!(pending.call(id).await.is_none());
}

#[tokio::test]
async fn provider_tool_use_waits_for_client_result_then_calls_provider_again() {
    let (directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ResponseEvent::Start {
            model_call_id: "ignored".into(),
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
    provider.push(vec![
        ResponseEvent::Start {
            model_call_id: "ignored".into(),
        },
        ResponseEvent::TextStart,
        ResponseEvent::TextDelta("done".into()),
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
    let handle = registry.get_or_create("tool-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(RunCommand::Append {
            seqno: 0,
            message: Box::new(client_run()),
        })
        .await
        .unwrap();
    let mut seqno = 1;
    let mut saw_exec = false;
    let mut saw_typed_completion = false;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                json!({})
            );
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                handle
                    .command(RunCommand::Append {
                        seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                seqno += 1;
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                saw_exec = true;
                let exec_id = exec.id;
                handle
                    .command(RunCommand::Append {
                        seqno,
                        message: Box::new(pb::AgentClientMessage {
                            message: Some(pb::agent_client_message::Message::ExecClientMessage(
                                pb::ExecClientMessage {
                                    id: exec_id,
                                    exec_id: String::new(),
                                    message: Some(pb::exec_client_message::Message::ReadResult(
                                        pb::ReadResult {
                                            result: Some(pb::read_result::Result::Success(
                                                pb::ReadSuccess {
                                                    path: "/tmp/a".into(),
                                                    total_lines: 1,
                                                    file_size: 1,
                                                    output: Some(
                                                        pb::read_success::Output::Content(
                                                            "x".into(),
                                                        ),
                                                    ),
                                                    ..Default::default()
                                                },
                                            )),
                                        },
                                    )),
                                    ..Default::default()
                                },
                            )),
                        }),
                    })
                    .await
                    .unwrap();
                seqno += 1;
                handle
                    .command(RunCommand::Append {
                        seqno,
                        message: Box::new(pb::AgentClientMessage {
                            message: Some(
                                pb::agent_client_message::Message::ExecClientControlMessage(
                                    pb::ExecClientControlMessage {
                                        message: Some(
                                            pb::exec_client_control_message::Message::StreamClose(
                                                pb::ExecClientStreamClose { id: exec_id },
                                            ),
                                        ),
                                    },
                                ),
                            ),
                        }),
                    })
                    .await
                    .unwrap();
                seqno += 1;
            }
            Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                if let Some(pb::interaction_update::Message::ToolCallCompleted(completed)) =
                    update.message
                {
                    let tool_call = completed.tool_call.expect("completed ToolCall");
                    assert!(tool_call.started_at_ms.unwrap_or_default() > 1);
                    assert!(tool_call.completed_at_ms.unwrap_or_default() > 1);
                    assert!(tool_call.completed_at_ms >= tool_call.started_at_ms);
                    let Some(pb::tool_call::Tool::ReadToolCall(read)) = tool_call.tool else {
                        panic!("expected completed ReadToolCall")
                    };
                    let result = read.result.expect("typed ReadToolResult");
                    assert!(matches!(
                        result.result,
                        Some(pb::read_tool_result::Result::Success(_))
                    ));
                    saw_typed_completion = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_exec);
    assert!(saw_typed_completion);
    assert_eq!(provider.requests().len(), 2);
    let database = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        directory.path().join("test.db").display()
    ))
    .await
    .unwrap();
    let provider_call_index: i64 =
        sqlx::query_scalar("SELECT provider_call_index FROM runs WHERE request_id = ?")
            .bind("tool-request")
            .fetch_one(&database)
            .await
            .unwrap();
    assert_eq!(provider_call_index, 1);
    let messages = store.load_messages("tool-conversation").await.unwrap();
    let result_position = messages
        .iter()
        .position(|message| matches!(message.content, MessageContent::ToolResult(_)))
        .expect("tool result persisted");
    let MessageContent::Assistant { tool_calls, .. } = &messages[result_position - 1].content
    else {
        panic!("tool result must immediately follow its assistant tool call")
    };
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].call_id, "call-1");
}

fn client_run() -> pb::AgentClientMessage {
    let user = pb::UserMessage {
        text: "read it".into(),
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
                conversation_id: Some("tool-conversation".into()),
                run_id: Some("tool-request".into()),
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
