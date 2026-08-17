use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Instant,
};

use tokio::sync::Mutex;

use crate::{cursor::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::edit::EditWrite;

#[derive(Clone, Default)]
pub struct CursorToolRuntime {
    next_id: Arc<AtomicU32>,
    execs: Arc<Mutex<HashMap<u32, PendingExec>>>,
    interactions: Arc<Mutex<HashMap<u32, PendingInteraction>>>,
    completed: Arc<Mutex<HashMap<u32, String>>>,
    mcp_tools: Arc<Mutex<HashMap<(String, String), pb::McpToolDefinition>>>,
}

pub(crate) struct PendingExec {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stage: ExecStage,
}

pub(crate) enum ExecStage {
    Direct,
    EditRead,
    EditWrite(EditWrite),
    Await(AwaitState),
}

pub(crate) struct AwaitState {
    pub deadline: Instant,
    pub output_file_path: String,
    pub task_id: String,
    pub regex: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExecContext {
    pub conversation_id: String,
    pub root_conversation_id: String,
    pub model_id: String,
    pub subagent_models: HashMap<String, SubagentModel>,
    pub terminals_folder: String,
    pub admin_command_denylist: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum SubagentModel {
    Model(String),
    Disabled,
}

pub(crate) struct PendingInteraction {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
}

impl CursorToolRuntime {
    pub async fn reserve_exec(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Direct, None)
            .await
    }

    pub(crate) async fn reserve_edit_read(
        &self,
        call: &ToolCall,
        context: &ExecContext,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::EditRead, None)
            .await
    }

    pub(crate) async fn reserve_edit_write(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        write: EditWrite,
        started_at_ms: u64,
    ) -> Result<u32> {
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::EditWrite(write),
            Some(started_at_ms),
        )
        .await
    }

    pub(crate) async fn reserve_await(
        &self,
        call: &ToolCall,
        context: &ExecContext,
    ) -> Result<u32> {
        let task_id = call
            .arguments
            .get("shell_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Protocol("AwaitShell is missing shell_id".into()))?;
        let block_ms = call
            .arguments
            .get("block_until_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(30_000);
        if block_ms > 7_140_000 {
            return Err(Error::Protocol(
                "AwaitShell block_until_ms exceeds 7140000".into(),
            ));
        }
        let output_file_path = format!(
            "{}/{}.txt",
            context.terminals_folder.trim_end_matches('/'),
            task_id
        );
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::Await(AwaitState {
                deadline: Instant::now() + std::time::Duration::from_millis(block_ms),
                output_file_path,
                task_id: task_id.to_string(),
                regex: call
                    .arguments
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            None,
        )
        .await
    }

    pub(crate) async fn reserve_await_again(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        state: AwaitState,
        started_at_ms: u64,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Await(state), Some(started_at_ms))
            .await
    }

    async fn reserve_exec_stage(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        stage: ExecStage,
        started_at_ms: Option<u64>,
    ) -> Result<u32> {
        let id = self.next_id()?;
        self.execs.lock().await.insert(
            id,
            PendingExec {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: started_at_ms.unwrap_or_else(now_ms),
                stdout: String::new(),
                stderr: String::new(),
                stage,
            },
        );
        Ok(id)
    }

    pub async fn reserve_interaction(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        let id = self.next_id()?;
        self.interactions.lock().await.insert(
            id,
            PendingInteraction {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: now_ms(),
            },
        );
        Ok(id)
    }

    pub async fn exec_call(&self, id: u32) -> Option<ToolCall> {
        self.execs
            .lock()
            .await
            .get(&id)
            .map(|entry| entry.call.clone())
    }

    pub async fn append_stdout(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stdout.push_str(data);
        true
    }

    pub async fn append_stderr(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stderr.push_str(data);
        true
    }

    pub(crate) async fn take_exec(&self, id: u32) -> Option<PendingExec> {
        let pending = self.execs.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub(crate) async fn take_interaction(&self, id: u32) -> Option<PendingInteraction> {
        let pending = self.interactions.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub async fn completed_call(&self, id: u32) -> Option<String> {
        self.completed.lock().await.get(&id).cloned()
    }

    pub(crate) async fn remember_mcp_state(
        &self,
        call: &ToolCall,
        result: &pb::McpStateExecResult,
    ) {
        let Some(pb::mcp_state_exec_result::Result::Success(success)) = result.result.as_ref()
        else {
            return;
        };
        let server_filter = call
            .arguments
            .get("server")
            .and_then(serde_json::Value::as_str);
        let mut tools = self.mcp_tools.lock().await;
        match server_filter {
            Some(server) => tools.retain(|(known_server, _), _| known_server != server),
            None => tools.clear(),
        }
        for server in &success.servers {
            for tool in &server.tools {
                tools.insert(
                    (server.server_identifier.clone(), tool.tool_name.clone()),
                    tool.clone(),
                );
            }
        }
    }

    pub(crate) async fn mcp_tool(&self, server: &str, tool: &str) -> Option<pb::McpToolDefinition> {
        self.mcp_tools
            .lock()
            .await
            .get(&(server.to_string(), tool.to_string()))
            .cloned()
    }

    pub async fn clear_completed(&self) {
        self.completed.lock().await.clear();
    }

    pub async fn discard_exec(&self, id: u32) {
        self.execs.lock().await.remove(&id);
    }

    pub async fn discard_interaction(&self, id: u32) {
        self.interactions.lock().await.remove(&id);
    }

    pub async fn drain_running(&self) -> Vec<u32> {
        let mut entries = self.execs.lock().await;
        let mut ids = entries.drain().map(|(id, _)| id).collect::<Vec<_>>();
        ids.sort_unstable();
        self.interactions.lock().await.clear();
        self.completed.lock().await.clear();
        self.mcp_tools.lock().await.clear();
        ids
    }

    fn next_id(&self) -> Result<u32> {
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("Cursor message id space exhausted".into()))
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
