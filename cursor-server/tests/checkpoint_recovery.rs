#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use cursor_server::{
    cursor::{connect, proto::agent::v1 as pb},
    prompting::{PromptAssets, PromptCompiler},
    run::RunRegistry,
    store::BlobId,
};
use prost::Message;

#[tokio::test]
async fn checkpoint_dependency_is_not_ready_until_blob_ack() {
    let (_directory, store) = fixtures::temp_store().await;
    store.create_pending_run("request").await.unwrap();
    let id = BlobId::digest(b"tool result");
    let key = format!("blob:{}", id.to_base64());
    store
        .enqueue_outbox("request", &key, "kv_set", b"tool result", &[])
        .await
        .unwrap();
    assert!(!store
        .dependencies_acked("request", std::slice::from_ref(&id))
        .await
        .unwrap());
    assert!(store.ack_outbox("request", &key).await.unwrap());
    assert!(store.dependencies_acked("request", &[id]).await.unwrap());
}

#[tokio::test]
async fn pending_blob_outbox_is_replayed_when_the_run_is_recreated() {
    let (_directory, store) = fixtures::temp_store().await;
    store.create_pending_run("recover-request").await.unwrap();
    let data = b"durable tool result";
    let blob_id = store.put_blob(data, &[]).await.unwrap();
    store
        .enqueue_outbox(
            "recover-request",
            &format!("blob:{}", blob_id.to_base64()),
            "kv_set",
            data,
            &[],
        )
        .await
        .unwrap();

    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt")
            .as_path(),
    )
    .unwrap();
    let registry = RunRegistry::new(
        store,
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
        "test".into(),
    );
    let handle = registry.get_or_create("recover-request").await.unwrap();
    let mut output = handle.subscribe();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), output.recv())
        .await
        .unwrap()
        .unwrap();
    let (_, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
    let message = pb::AgentServerMessage::decode(payload).unwrap();
    let Some(pb::agent_server_message::Message::KvServerMessage(kv)) = message.message else {
        panic!("expected replayed KV SET")
    };
    let Some(pb::kv_server_message::Message::SetBlobArgs(set)) = kv.message else {
        panic!("expected SetBlobArgs")
    };
    assert_eq!(set.blob_id, blob_id.as_bytes());
    assert_eq!(set.blob_data, data);
}
