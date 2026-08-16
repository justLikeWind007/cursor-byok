#[path = "support/fixtures.rs"]
mod fixtures;

use cursor_server::model::RuntimeEvent;

#[tokio::test]
async fn runtime_event_is_appended_exactly_once() {
    let (_directory, store) = fixtures::temp_store().await;
    let event = RuntimeEvent {
        event_id: "branch:changed:7".into(),
        text: "runtime state changed".into(),
    };
    assert!(store
        .append_runtime_event_once("conversation", event.clone())
        .await
        .unwrap());
    assert!(!store
        .append_runtime_event_once("conversation", event)
        .await
        .unwrap());
    let messages = store.load_messages("conversation").await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].runtime_event_id.as_deref(),
        Some("branch:changed:7")
    );
}
