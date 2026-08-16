use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use prost::Message;
use tokio::sync::{oneshot, Mutex, Notify};

use crate::{
    cursor::proto::agent::v1 as pb,
    run::RunHandle,
    store::{BlobEdge, BlobId, Store},
    Error, Result,
};

type BlobGetSender = oneshot::Sender<Result<Option<Vec<u8>>>>;

#[derive(Clone)]
pub struct BlobSynchronizer {
    inner: Arc<Inner>,
}

struct Inner {
    request_id: String,
    store: Store,
    handle: RunHandle,
    next_id: AtomicU32,
    set_requests: Mutex<HashMap<u32, BlobId>>,
    get_requests: Mutex<HashMap<u32, BlobGetSender>>,
    ack: Notify,
}

impl BlobSynchronizer {
    pub fn new(request_id: String, store: Store, handle: RunHandle) -> Self {
        Self {
            inner: Arc::new(Inner {
                request_id,
                store,
                handle,
                next_id: AtomicU32::new(1),
                set_requests: Mutex::new(HashMap::new()),
                get_requests: Mutex::new(HashMap::new()),
                ack: Notify::new(),
            }),
        }
    }

    pub fn request_id(&self) -> &str {
        &self.inner.request_id
    }

    pub async fn recover(&self) -> Result<()> {
        for item in self
            .inner
            .store
            .pending_outbox(&self.inner.request_id)
            .await?
        {
            if item.kind != "kv_set" {
                continue;
            }
            let encoded = item
                .key
                .strip_prefix("blob:")
                .ok_or_else(|| Error::Protocol(format!("invalid Blob outbox key: {}", item.key)))?;
            let blob_id = BlobId::from_base64(encoded)?;
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            self.inner
                .set_requests
                .lock()
                .await
                .insert(id, blob_id.clone());
            self.inner.handle.emit(&pb::AgentServerMessage {
                ttft_breakdown: None,
                message: Some(pb::agent_server_message::Message::KvServerMessage(
                    pb::KvServerMessage {
                        id,
                        span_context: None,
                        message: Some(pb::kv_server_message::Message::SetBlobArgs(
                            pb::SetBlobArgs {
                                blob_id: blob_id.as_bytes().to_vec(),
                                blob_data: item.payload,
                            },
                        )),
                    },
                )),
            })?;
            self.inner.store.mark_outbox_sent(item.id).await?;
        }
        self.publish_ready_checkpoints().await
    }

    pub async fn persist(&self, data: &[u8], edges: &[BlobEdge]) -> Result<BlobId> {
        let id = self.inner.store.put_blob(data, edges).await?;
        let key = format!("blob:{}", id.to_base64());
        self.inner
            .store
            .enqueue_outbox(&self.inner.request_id, &key, "kv_set", data, &[])
            .await?;
        self.ensure_set(&id, data).await?;
        Ok(id)
    }

    async fn ensure_set(&self, blob_id: &BlobId, data: &[u8]) -> Result<()> {
        let dependency = [blob_id.clone()];
        loop {
            if self
                .inner
                .store
                .dependencies_acked(&self.inner.request_id, &dependency)
                .await?
            {
                return Ok(());
            }
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            self.inner
                .set_requests
                .lock()
                .await
                .insert(id, blob_id.clone());
            self.inner.handle.emit(&pb::AgentServerMessage {
                ttft_breakdown: None,
                message: Some(pb::agent_server_message::Message::KvServerMessage(
                    pb::KvServerMessage {
                        id,
                        span_context: None,
                        message: Some(pb::kv_server_message::Message::SetBlobArgs(
                            pb::SetBlobArgs {
                                blob_id: blob_id.as_bytes().to_vec(),
                                blob_data: data.to_vec(),
                            },
                        )),
                    },
                )),
            })?;
            let cancellation = self.inner.handle.cancellation();
            tokio::select! {
                _ = self.inner.ack.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
            }
        }
    }

    pub async fn get(&self, blob_id: &BlobId) -> Result<Option<Vec<u8>>> {
        if let Some(data) = self.inner.store.get_blob(blob_id).await? {
            return Ok(Some(data));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.inner.get_requests.lock().await.insert(id, sender);
        self.inner.handle.emit(&pb::AgentServerMessage {
            ttft_breakdown: None,
            message: Some(pb::agent_server_message::Message::KvServerMessage(
                pb::KvServerMessage {
                    id,
                    span_context: None,
                    message: Some(pb::kv_server_message::Message::GetBlobArgs(
                        pb::GetBlobArgs {
                            blob_id: blob_id.as_bytes().to_vec(),
                        },
                    )),
                },
            )),
        })?;
        let cancellation = self.inner.handle.cancellation();
        tokio::select! {
            result = receiver => result.map_err(|_| Error::Protocol("KV GET response channel closed".into()))?,
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(15)) => Err(Error::Protocol(format!("KV GET timed out: {}", blob_id.to_base64()))),
        }
    }

    pub async fn handle_client(&self, message: pb::KvClientMessage) -> Result<()> {
        match message.message {
            Some(pb::kv_client_message::Message::SetBlobResult(result)) => {
                if result.error.is_none() {
                    if let Some(blob_id) = self.inner.set_requests.lock().await.remove(&message.id)
                    {
                        self.inner
                            .store
                            .ack_outbox(
                                &self.inner.request_id,
                                &format!("blob:{}", blob_id.to_base64()),
                            )
                            .await?;
                        self.inner.ack.notify_waiters();
                    }
                }
            }
            Some(pb::kv_client_message::Message::GetBlobResult(result)) => {
                if let Some(sender) = self.inner.get_requests.lock().await.remove(&message.id) {
                    let value = if let Some(error) = result.error {
                        Err(Error::Protocol(format!("KV GET: {}", error.message)))
                    } else {
                        Ok(result.blob_data)
                    };
                    let _ = sender.send(value);
                }
            }
            None => {}
        }
        self.publish_ready_checkpoints().await?;
        Ok(())
    }

    async fn publish_ready_checkpoints(&self) -> Result<()> {
        for item in self
            .inner
            .store
            .pending_outbox(&self.inner.request_id)
            .await?
        {
            if item.kind != "checkpoint" {
                continue;
            }
            let dependencies = item
                .dependencies
                .iter()
                .map(|id| BlobId::from_base64(id))
                .collect::<Result<Vec<_>>>()?;
            if !self
                .inner
                .store
                .dependencies_acked(&self.inner.request_id, &dependencies)
                .await?
            {
                continue;
            }
            let checkpoint = pb::ConversationStateStructure::decode(item.payload.as_slice())?;
            self.inner.handle.emit(&pb::AgentServerMessage {
                ttft_breakdown: None,
                message: Some(
                    pb::agent_server_message::Message::ConversationCheckpointUpdate(checkpoint),
                ),
            })?;
            self.inner
                .store
                .ack_outbox(&self.inner.request_id, &item.key)
                .await?;
        }
        Ok(())
    }
}
