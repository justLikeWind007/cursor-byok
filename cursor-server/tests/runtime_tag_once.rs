#[path = "support/fixtures.rs"]
mod fixtures;

use cursor_server::model::{
    ConversationId, ModelSpec, PreparedRun, PromptSpec, RunAction, RunId, RunKind, RuntimeEvent,
};

#[tokio::test]
async fn runtime_event_is_appended_exactly_once() {
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
    let message = RuntimeEvent {
        event_id: "branch:changed:7".into(),
        text: "runtime state changed".into(),
    }
    .into_message();

    let (revision, inserted) = store
        .append_message_once(&conversation_id, &run.run_id, root, &message)
        .await
        .unwrap();
    assert!(inserted);
    assert!(
        !store
            .append_message_once(&conversation_id, &run.run_id, revision, &message)
            .await
            .unwrap()
            .1
    );
    let messages = store.load_revision_messages(revision).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].runtime_event_id.as_deref(),
        Some("branch:changed:7")
    );
}
