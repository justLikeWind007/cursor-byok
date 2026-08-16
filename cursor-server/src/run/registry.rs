use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{blob_sync::BlobSynchronizer, proto::agent::v1 as pb},
    prompting::PromptCompiler,
    provider::Provider,
    store::Store,
    Result,
};

use super::{actor::RunDependencies, RunActor, RunCommand};

#[derive(Clone)]
pub struct RunHandle {
    request_id: String,
    commands: mpsc::Sender<RunCommand>,
    output: Arc<OutputHub>,
    cancellation: CancellationToken,
}

impl RunHandle {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Bytes> {
        self.output.subscribe()
    }
    pub async fn command(&self, command: RunCommand) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| crate::Error::RunNotFound(self.request_id.clone()))
    }
    pub fn emit_frame(&self, frame: Bytes) {
        self.output.emit(frame);
    }
    pub fn emit(&self, message: &pb::AgentServerMessage) -> Result<()> {
        self.emit_frame(crate::cursor::connect::encode_message(message)?);
        Ok(())
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub fn close_output(&self) {
        self.output.close();
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Default)]
struct OutputHub {
    state: std::sync::Mutex<OutputState>,
}

#[derive(Default)]
struct OutputState {
    history: Vec<Bytes>,
    subscribers: Vec<mpsc::UnboundedSender<Bytes>>,
    closed: bool,
}

impl OutputHub {
    fn emit(&self, frame: Bytes) {
        let mut state = self.state.lock().expect("output hub lock");
        if state.closed {
            return;
        }
        state.history.push(frame.clone());
        state
            .subscribers
            .retain(|subscriber| subscriber.send(frame.clone()).is_ok());
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<Bytes> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock().expect("output hub lock");
        for frame in &state.history {
            let _ = sender.send(frame.clone());
        }
        if !state.closed {
            state.subscribers.push(sender);
        }
        receiver
    }

    fn close(&self) {
        let mut state = self.state.lock().expect("output hub lock");
        state.closed = true;
        state.subscribers.clear();
    }
}

#[derive(Clone)]
pub struct RunRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    runs: Mutex<HashMap<String, RunHandle>>,
    conversations: Mutex<HashMap<String, String>>,
    store: Store,
    provider: Arc<dyn Provider>,
    compiler: PromptCompiler,
    model: String,
}

impl RunRegistry {
    pub fn new(
        store: Store,
        provider: Arc<dyn Provider>,
        compiler: PromptCompiler,
        model: String,
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                runs: Mutex::new(HashMap::new()),
                conversations: Mutex::new(HashMap::new()),
                store,
                provider,
                compiler,
                model,
            }),
        }
    }

    pub async fn get_or_create(&self, request_id: &str) -> Result<RunHandle> {
        if let Some(handle) = self.inner.runs.lock().await.get(request_id).cloned() {
            return Ok(handle);
        }
        self.inner.store.create_pending_run(request_id).await?;
        let next_append_seqno = self.inner.store.next_append_seqno(request_id).await?;
        let (commands, receiver) = mpsc::channel(128);
        let output = Arc::new(OutputHub::default());
        let cancellation = CancellationToken::new();
        let handle = RunHandle {
            request_id: request_id.into(),
            commands,
            output,
            cancellation,
        };
        let mut runs = self.inner.runs.lock().await;
        if let Some(existing) = runs.get(request_id).cloned() {
            return Ok(existing);
        }
        runs.insert(request_id.into(), handle.clone());
        drop(runs);
        let blob_sync =
            BlobSynchronizer::new(request_id.into(), self.inner.store.clone(), handle.clone());
        let recovery = blob_sync.clone();
        tokio::spawn(async move {
            if let Err(error) = recovery.recover().await {
                tracing::error!(%error, "failed to replay Run outbox");
            }
        });
        RunActor::spawn(
            handle.clone(),
            receiver,
            RunDependencies {
                store: self.inner.store.clone(),
                provider: self.inner.provider.clone(),
                compiler: self.inner.compiler.clone(),
                model: self.inner.model.clone(),
            },
            blob_sync,
            next_append_seqno,
        );
        Ok(handle)
    }

    pub async fn bind_conversation(&self, conversation_id: &str, request_id: &str) {
        let previous = self
            .inner
            .conversations
            .lock()
            .await
            .insert(conversation_id.into(), request_id.into());
        if let Some(previous) = previous.filter(|previous| previous != request_id) {
            if let Some(handle) = self.inner.runs.lock().await.get(&previous).cloned() {
                handle.cancel();
            }
        }
    }

    pub async fn shutdown(&self) {
        let handles = {
            let mut runs = self.inner.runs.lock().await;
            runs.drain().map(|(_, handle)| handle).collect::<Vec<_>>()
        };
        self.inner.conversations.lock().await.clear();
        for handle in handles {
            handle.cancel();
        }
    }
}
