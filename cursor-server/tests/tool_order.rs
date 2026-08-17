#[path = "support/fixtures.rs"]
mod fixtures;

use cursor_server::{
    model::{
        ConversationId, MessageContent, ModelSpec, PreparedRun, PromptSpec, RunAction, RunId,
        RunKind, ToolCall, ToolResult, ToolRoundAssistant, ToolRoundId,
    },
    store::ToolRoundStatus,
};

#[tokio::test]
async fn results_commit_adjacent_pairs_in_arrival_order() {
    let (_directory, store) = fixtures::temp_store().await;
    let conversation_id = ConversationId::new("conversation");
    let root = store.ensure_conversation(&conversation_id).await.unwrap();
    let run = PreparedRun {
        run_id: RunId::new("run"),
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
        base_revision_id: root,
    };
    store.claim_run(&run).await.unwrap();
    let round_id = ToolRoundId::new("round");
    let calls = [call(0, "A"), call(1, "B"), call(2, "C")];
    store
        .create_tool_round(
            &round_id,
            &run.run_id,
            root,
            &ToolRoundAssistant {
                text: "answer prefix".into(),
                thinking: "reasoning".into(),
                model_call_id: "model-call".into(),
                replay_state: None,
            },
            &calls,
            None,
        )
        .await
        .unwrap();

    let b = store
        .commit_tool_result(&conversation_id, &run.run_id, &round_id, &result("B"))
        .await
        .unwrap();
    assert_eq!(b.completion_seq, 0);
    assert!(!b.settled);
    let a = store
        .commit_tool_result(&conversation_id, &run.run_id, &round_id, &result("A"))
        .await
        .unwrap();
    assert_eq!(a.completion_seq, 1);
    let c = store
        .commit_tool_result(&conversation_id, &run.run_id, &round_id, &result("C"))
        .await
        .unwrap();
    assert!(c.settled);

    let messages = store.load_revision_messages(c.revision_id).await.unwrap();
    assert_eq!(messages.len(), 6);
    let ids = messages
        .chunks_exact(2)
        .map(|pair| match (&pair[0].content, &pair[1].content) {
            (MessageContent::Assistant { tool_calls, .. }, MessageContent::ToolResult(result)) => {
                assert_eq!(tool_calls[0].call_id, result.call_id);
                result.call_id.clone()
            }
            _ => panic!("tool result must be adjacent to its assistant call"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["B", "A", "C"]);
    let MessageContent::Assistant { text, thinking, .. } = &messages[0].content else {
        unreachable!()
    };
    assert_eq!(text, "answer prefix");
    assert_eq!(thinking, "reasoning");
    for message in [&messages[2], &messages[4]] {
        let MessageContent::Assistant { text, thinking, .. } = &message.content else {
            unreachable!()
        };
        assert!(text.is_empty());
        assert!(thinking.is_empty());
    }
    let snapshot = store.tool_round(&round_id).await.unwrap().unwrap();
    assert_eq!(snapshot.status, ToolRoundStatus::Settled);
    assert_eq!(snapshot.completed_call_ids.len(), 3);
}

fn call(index: usize, call_id: &str) -> ToolCall {
    ToolCall {
        index,
        call_id: call_id.into(),
        model_call_id: "model-call".into(),
        name: "Read".into(),
        arguments_text: r#"{"path":"/tmp/a"}"#.into(),
        arguments: serde_json::json!({"path":"/tmp/a"}),
    }
}

fn result(call_id: &str) -> ToolResult {
    ToolResult {
        call_id: call_id.into(),
        content: format!("result-{call_id}"),
        is_error: false,
    }
}
