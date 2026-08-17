use std::collections::{BTreeMap, HashMap, HashSet};

use cursor_server::{
    cursor::{
        interaction,
        proto::agent::v1 as pb,
        tools::{
            codec,
            runtime::{CursorToolRuntime, ExecContext, SubagentModel},
            ToolBatchState, ToolDispatcher,
        },
    },
    model::{CanonicalMessage, MessageContent, Origin, Role, ToolCall},
};

#[test]
fn task_keeps_wire_type_model_parent_and_background_fields() {
    let mut context = context();
    context.subagent_models.insert(
        "cursor-guide".into(),
        SubagentModel::Model("guide-model".into()),
    );
    let call = task_call(serde_json::json!({
        "description": "guide",
        "prompt": "inspect",
        "subagent_type": "cursor-guide",
        "run_in_background": true,
        "interrupt": false,
    }));

    let message = codec::request(7, &call, &context).unwrap();
    let pb::agent_server_message::Message::ExecServerMessage(exec) = message.message.unwrap()
    else {
        panic!("expected ExecServerMessage")
    };
    assert_eq!(exec.id, 7);
    assert_eq!(exec.exec_id, "task-call");
    assert_eq!(exec.accept_hook_additional_contexts, Some(false));
    let pb::exec_server_message::Message::SubagentArgs(args) = exec.message.unwrap() else {
        panic!("expected SubagentArgs")
    };
    assert_eq!(args.subagent_type, "cursor-guide");
    assert_eq!(args.model_id, "guide-model");
    assert_eq!(args.parent_conversation_id.as_deref(), Some("child"));
    assert_eq!(args.root_parent_conversation_id.as_deref(), Some("root"));
    assert_eq!(args.run_in_background, Some(true));
    assert_eq!(args.interrupt, Some(false));
}

#[test]
fn task_renders_general_typed_and_custom_subagent_types_without_aliases() {
    let cases = [
        ("generalPurpose", "unspecified"),
        ("cursor-guide", "cursor-guide"),
        ("MyReviewer", "MyReviewer"),
    ];
    for (name, expected) in cases {
        let call = task_call(serde_json::json!({
            "description": "task",
            "prompt": "inspect",
            "subagent_type": name,
        }));
        let rendered = interaction::render_tool_call(&call, false).unwrap();
        let Some(pb::tool_call::Tool::TaskToolCall(tool)) = rendered.tool else {
            panic!("expected TaskToolCall")
        };
        let subagent = tool.args.unwrap().subagent_type.unwrap().r#type.unwrap();
        match (expected, subagent) {
            ("unspecified", pb::subagent_type::Type::Unspecified(_)) => {}
            ("cursor-guide", pb::subagent_type::Type::CursorGuide(_)) => {}
            (custom, pb::subagent_type::Type::Custom(value)) => {
                assert_eq!(value.name, custom)
            }
            _ => panic!("wrong subagent oneof for {name}"),
        }
    }
}

#[test]
fn disabled_task_model_is_a_protocol_error() {
    let mut context = context();
    context
        .subagent_models
        .insert("security-review".into(), SubagentModel::Disabled);
    let call = task_call(serde_json::json!({
        "description": "review",
        "prompt": "inspect",
        "subagent_type": "security-review",
    }));
    assert!(codec::request(1, &call, &context)
        .unwrap_err()
        .to_string()
        .contains("security-review is disabled"));
}

#[tokio::test]
async fn update_current_step_uses_a_one_based_turn_message_index() {
    let dispatcher = ToolDispatcher::new(CursorToolRuntime::default());
    let call = ToolCall {
        index: 0,
        call_id: "update-call".into(),
        model_call_id: "model-call".into(),
        name: "UpdateCurrentStep".into(),
        arguments_text: r#"{"current_step":"testing"}"#.into(),
        arguments: serde_json::json!({"current_step":"testing"}),
    };
    let completed = HashSet::new();
    let started = HashSet::new();
    let messages = vec![
        CanonicalMessage::text("old-runtime", Role::User, Origin::Runtime, "old turn"),
        CanonicalMessage {
            message_id: "old-assistant".into(),
            role: Role::Assistant,
            origin: Origin::Assistant,
            content: MessageContent::Assistant {
                text: "old response".into(),
                thinking: String::new(),
                tool_round_id: None,
                replay_state: None,
                tool_calls: Vec::new(),
            },
            runtime_event_id: None,
        },
        CanonicalMessage::text(
            "current-runtime",
            Role::User,
            Origin::Runtime,
            "current turn",
        ),
    ];
    let dispatched = dispatcher
        .start_batch(
            &[call],
            ToolBatchState {
                completed: &completed,
                started: &started,
                response_text: "",
                response_thinking: "",
            },
            &messages,
            &BTreeMap::new(),
            &context(),
        )
        .await
        .unwrap();
    let completion = dispatched[0].completion.as_ref().unwrap();
    let Some(pb::tool_call::Tool::CommunicateUpdateToolCall(tool)) =
        completion.tool_call().tool.as_ref()
    else {
        panic!("expected CommunicateUpdateToolCall")
    };
    let pb::communicate_update_result::Result::Success(success) =
        tool.result.as_ref().unwrap().result.as_ref().unwrap()
    else {
        panic!("expected communicate update success")
    };
    assert_eq!(success.message_index, 1);
    assert_eq!(success.current_step, "testing");
}

fn context() -> ExecContext {
    ExecContext {
        conversation_id: "child".into(),
        root_conversation_id: "root".into(),
        model_id: "parent-model".into(),
        subagent_models: HashMap::new(),
        terminals_folder: "/tmp/terminals".into(),
        admin_command_denylist: Vec::new(),
    }
}

fn task_call(arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        index: 0,
        call_id: "task-call".into(),
        model_call_id: "model-call".into(),
        name: "Task".into(),
        arguments_text: arguments.to_string(),
        arguments,
    }
}
