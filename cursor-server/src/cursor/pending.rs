use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use tokio::sync::Mutex;

use crate::{model::ToolCall, Error, Result};

#[derive(Clone, Default)]
pub struct PendingExecRegistry {
    next_id: Arc<AtomicU32>,
    entries: Arc<Mutex<HashMap<u32, PendingExec>>>,
}

pub(crate) struct PendingExec {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Default)]
pub struct ExecContext {
    pub conversation_id: String,
    pub terminals_folder: String,
    pub admin_command_denylist: Vec<String>,
}

#[derive(Clone, Default)]
pub struct PendingClientTools {
    next_id: Arc<AtomicU32>,
    calls: Arc<Mutex<HashMap<u32, PendingClientTool>>>,
}

pub(crate) struct PendingClientTool {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
}

impl PendingExecRegistry {
    pub async fn reserve(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        let id = next_id(&self.next_id)?;
        self.entries.lock().await.insert(
            id,
            PendingExec {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: now_ms(),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        Ok(id)
    }

    pub async fn call(&self, id: u32) -> Option<ToolCall> {
        self.entries
            .lock()
            .await
            .get(&id)
            .map(|entry| entry.call.clone())
    }

    pub async fn append_stdout(&self, id: u32, data: &str) -> bool {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stdout.push_str(data);
        true
    }

    pub async fn append_stderr(&self, id: u32, data: &str) -> bool {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stderr.push_str(data);
        true
    }

    pub(crate) async fn take(&self, id: u32) -> Option<PendingExec> {
        self.entries.lock().await.remove(&id)
    }

    pub async fn discard(&self, id: u32) {
        self.entries.lock().await.remove(&id);
    }

    pub async fn drain_running(&self) -> Vec<u32> {
        let mut entries = self.entries.lock().await;
        let mut ids = entries.drain().map(|(id, _)| id).collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }
}

impl PendingClientTools {
    pub async fn reserve(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        let id = next_id(&self.next_id)?;
        self.calls.lock().await.insert(
            id,
            PendingClientTool {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: now_ms(),
            },
        );
        Ok(id)
    }

    pub(crate) async fn take(&self, id: u32) -> Option<PendingClientTool> {
        self.calls.lock().await.remove(&id)
    }

    pub async fn discard(&self, id: u32) {
        self.calls.lock().await.remove(&id);
    }
}

fn next_id(counter: &AtomicU32) -> Result<u32> {
    counter
        .fetch_add(1, Ordering::Relaxed)
        .checked_add(1)
        .ok_or_else(|| Error::Protocol("Cursor message id space exhausted".into()))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
