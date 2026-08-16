#[path = "support/fixtures.rs"]
mod fixtures;

use cursor_server::{
    model::{CanonicalMessage, MessageContent, Origin, Role, ToolCallContent, ToolResultContent},
    prompting::{project_messages, Mode, PromptAssets, PromptCompiler, ToolDefinition},
};

#[test]
fn projecting_an_append_only_context_preserves_the_complete_prefix() {
    let first = vec![fixtures::user("u1", "one")];
    let mut second = first.clone();
    second.push(fixtures::user("u2", "two"));
    let projected_first = project_messages(&first).unwrap();
    let projected_second = project_messages(&second).unwrap();
    assert_eq!(projected_first, projected_second[..projected_first.len()]);
}

#[test]
fn every_tool_result_is_projected_as_string_content() {
    let object = serde_json::json!({"merge": false, "todos": []});
    let messages = vec![
        tool_result("object", object.clone()),
        tool_result("string", serde_json::Value::String("plain text".into())),
    ];
    let projected = project_messages(&messages).unwrap();

    let object_text = projected[0]
        .content
        .as_str()
        .expect("object ToolResult must be JSON-encoded into a string");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(object_text).unwrap(),
        object
    );
    assert_eq!(projected[1].content.as_str(), Some("plain text"));
}

#[test]
fn assistant_text_and_thinking_remain_separate_during_projection() {
    let messages = vec![CanonicalMessage {
        message_id: "assistant".into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: "visible answer".into(),
            thinking: "private reasoning".into(),
            model_call_id: Some("model-call".into()),
            tool_calls: Vec::new(),
        },
        runtime_event_id: None,
    }];

    let projected = project_messages(&messages).unwrap();
    assert_eq!(projected[0].content.as_str(), Some("visible answer"));
    assert_eq!(projected[0].thinking.as_deref(), Some("private reasoning"));
}

#[test]
fn split_tool_pairs_reconstruct_the_original_provider_assistant_message() {
    let messages = vec![
        assistant_tool_pair(
            "assistant-second",
            "model-call",
            1,
            "call-second",
            "visible answer",
            "complete reasoning",
        ),
        tool_result_with_call("result-second", "call-second", "second"),
        assistant_tool_pair("assistant-first", "model-call", 0, "call-first", "", ""),
        tool_result_with_call("result-first", "call-first", "first"),
    ];

    let projected = project_messages(&messages).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0].role, "assistant");
    assert_eq!(projected[0].thinking.as_deref(), Some("complete reasoning"));
    let calls = projected[0].tool_calls.as_ref().unwrap();
    assert_eq!(calls[0]["id"], "call-first");
    assert_eq!(calls[1]["id"], "call-second");
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-first"));
    assert_eq!(projected[2].tool_call_id.as_deref(), Some("call-second"));
}

#[test]
fn every_prompt_mode_loads_the_captured_tool_set() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    assert_eq!(assets.mode(Mode::Agent).tools.len(), 20);
    assert_eq!(assets.mode(Mode::Ask).tools.len(), 18);
    assert_eq!(assets.mode(Mode::Plan).tools.len(), 16);
    assert_eq!(assets.mode(Mode::Debug).tools.len(), 18);
    assert_eq!(assets.mode(Mode::Multitask).tools.len(), 20);
    assert_eq!(assets.mode(Mode::Subagent).tools.len(), 4);
}

#[test]
fn dynamic_mcp_tools_are_appended_after_the_stable_mode_tool_prefix() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let messages = vec![fixtures::user("u1", "one")];
    let base = compiler
        .compile(Mode::Agent, "model", "call", &messages)
        .unwrap();
    let dynamic = compiler
        .compile_with_dynamic_tools(
            Mode::Agent,
            "model",
            "call",
            &messages,
            &[ToolDefinition {
                name: "mcp_repo_lookup".into(),
                description: "lookup".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        )
        .unwrap();
    assert_eq!(base.tools, dynamic.tools[..base.tools.len()]);
    assert_eq!(dynamic.tools.last().unwrap().name, "mcp_repo_lookup");
}

fn tool_result(id: &str, output: serde_json::Value) -> CanonicalMessage {
    tool_result_with_call(id, &format!("call-{id}"), output)
}

fn tool_result_with_call(
    id: &str,
    call_id: &str,
    output: impl Into<serde_json::Value>,
) -> CanonicalMessage {
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Tool,
        origin: Origin::Tool,
        content: MessageContent::ToolResult(ToolResultContent {
            call_id: call_id.into(),
            name: "Tool".into(),
            output: output.into(),
            is_error: false,
        }),
        runtime_event_id: None,
    }
}

fn assistant_tool_pair(
    id: &str,
    model_call_id: &str,
    index: usize,
    call_id: &str,
    text: &str,
    thinking: &str,
) -> CanonicalMessage {
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: text.into(),
            thinking: thinking.into(),
            model_call_id: Some(model_call_id.into()),
            tool_calls: vec![ToolCallContent {
                index,
                call_id: call_id.into(),
                name: "Tool".into(),
                arguments: serde_json::json!({}),
            }],
        },
        runtime_event_id: None,
    }
}
