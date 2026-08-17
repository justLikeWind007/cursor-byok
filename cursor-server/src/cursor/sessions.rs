use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use bytes::Bytes;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::prompting::PromptCompiler,
    cursor::{blob_sync::BlobSynchronizer, proto::agent::v1 as pb},
    provider::Provider,
    run::RunRegistry,
    store::Store,
    Result,
};

use super::{
    actor::{CursorActor, RunDependencies},
    CursorCommand,
};

#[derive(Clone)]
pub struct CursorSessionHandle {
    request_id: String,
    commands: mpsc::Sender<CursorCommand>,
    output: Arc<OutputHub>,
    cancellation: CancellationToken,
    parent: Arc<OnceLock<CursorParent>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorParent {
    pub run_id: String,
    pub tool_call_id: String,
}

impl CursorSessionHandle {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Bytes> {
        self.output.subscribe()
    }
    pub async fn command(&self, command: CursorCommand) -> Result<()> {
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
    pub fn set_parent(&self, parent: CursorParent) -> Result<()> {
        if parent.run_id.is_empty() || parent.tool_call_id.is_empty() {
            return Err(crate::Error::Protocol(
                "Cursor parent run and tool call ids are required".into(),
            ));
        }
        if self.parent.get().is_some_and(|current| current != &parent) {
            return Err(crate::Error::Protocol(format!(
                "conflicting parent ids for request {}",
                self.request_id
            )));
        }
        let _ = self.parent.set(parent);
        Ok(())
    }
    pub fn parent(&self) -> Option<&CursorParent> {
        self.parent.get()
    }
}

#[derive(Default)]
struct OutputHub {
    state: parking_lot::Mutex<OutputState>,
    closed: tokio::sync::Notify,
}

#[derive(Default)]
struct OutputState {
    history: Vec<Bytes>,
    subscribers: Vec<mpsc::UnboundedSender<Bytes>>,
    closed: bool,
}

impl OutputHub {
    fn emit(&self, frame: Bytes) {
        let mut state = self.state.lock();
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
        let mut state = self.state.lock();
        for frame in &state.history {
            let _ = sender.send(frame.clone());
        }
        if !state.closed {
            state.subscribers.push(sender);
        }
        receiver
    }

    fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.subscribers.clear();
        drop(state);
        self.closed.notify_waiters();
    }

    async fn wait_closed(&self) {
        loop {
            let notified = self.closed.notified();
            if self.state.lock().closed {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone)]
pub struct CursorSessionRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    runs: Mutex<HashMap<String, CursorSessionHandle>>,
    run_registry: RunRegistry,
    store: Store,
    provider: Arc<dyn Provider>,
    compiler: PromptCompiler,
}

impl CursorSessionRegistry {
    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn new(
        store: Store,
        provider: Arc<dyn Provider>,
        compiler: PromptCompiler,
        run_registry: RunRegistry,
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                runs: Mutex::new(HashMap::new()),
                run_registry,
                store,
                provider,
                compiler,
            }),
        }
    }

    pub async fn get_or_create(&self, request_id: &str) -> Result<CursorSessionHandle> {
        if let Some(handle) = self.inner.runs.lock().await.get(request_id).cloned() {
            return Ok(handle);
        }
        let (commands, receiver) = mpsc::channel(128);
        let output = Arc::new(OutputHub::default());
        let cancellation = CancellationToken::new();
        let handle = CursorSessionHandle {
            request_id: request_id.into(),
            commands,
            output,
            cancellation,
            parent: Arc::new(OnceLock::new()),
        };
        let mut runs = self.inner.runs.lock().await;
        if let Some(existing) = runs.get(request_id).cloned() {
            return Ok(existing);
        }
        runs.insert(request_id.into(), handle.clone());
        drop(runs);
        let blob_sync =
            BlobSynchronizer::new(request_id.into(), self.inner.store.clone(), handle.clone());
        CursorActor::spawn(
            handle.clone(),
            receiver,
            RunDependencies {
                store: self.inner.store.clone(),
                provider: self.inner.provider.clone(),
                compiler: self.inner.compiler.clone(),
                run_registry: self.inner.run_registry.clone(),
            },
            blob_sync,
            0,
        );
        let registry = Arc::downgrade(&self.inner);
        let request_id = request_id.to_string();
        let output = handle.output.clone();
        tokio::spawn(async move {
            output.wait_closed().await;
            let Some(registry) = registry.upgrade() else {
                return;
            };
            registry.runs.lock().await.remove(&request_id);
        });
        Ok(handle)
    }

    pub async fn shutdown(&self) {
        let handles = {
            let mut runs = self.inner.runs.lock().await;
            runs.drain().map(|(_, handle)| handle).collect::<Vec<_>>()
        };
        self.inner.run_registry.shutdown().await;
        for handle in handles {
            handle.cancel();
            let _ = crate::cursor::lifecycle::cancel(&handle);
        }
    }
}
