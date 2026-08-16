use sqlx::Row;

use crate::{
    model::{ToolResult, Usage},
    Result,
};

use super::{now_ms, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Waiting,
    Running,
    Completed,
    Interrupted,
    Failed,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

impl Store {
    pub async fn create_pending_run(&self, request_id: &str) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "INSERT OR IGNORE INTO runs(request_id, status, created_at_ms, updated_at_ms) VALUES (?, 'waiting', ?, ?)",
        )
        .bind(request_id).bind(now).bind(now)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn next_append_seqno(&self, request_id: &str) -> Result<i64> {
        let current: i64 = sqlx::query_scalar("SELECT append_seqno FROM runs WHERE request_id = ?")
            .bind(request_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(current + 1)
    }

    pub async fn bind_run(
        &self,
        request_id: &str,
        run_id: &str,
        conversation_id: &str,
        revision: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        Self::ensure_conversation_tx(&mut tx, conversation_id).await?;
        sqlx::query(
            "UPDATE runs SET run_id = ?, conversation_id = ?, revision = ?, status = 'running', updated_at_ms = ? WHERE request_id = ?",
        )
        .bind(run_id).bind(conversation_id).bind(revision).bind(now_ms()).bind(request_id)
        .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn advance_append_seqno(&self, request_id: &str, seqno: i64) -> Result<bool> {
        let changed = sqlx::query(
            "UPDATE runs SET append_seqno = ?, updated_at_ms = ? WHERE request_id = ? AND append_seqno < ?",
        )
        .bind(seqno).bind(now_ms()).bind(request_id).bind(seqno)
        .execute(&self.pool).await?.rows_affected() == 1;
        Ok(changed)
    }

    pub async fn update_run_status(
        &self,
        request_id: &str,
        status: RunStatus,
        usage: Usage,
    ) -> Result<()> {
        sqlx::query("UPDATE runs SET status = ?, turn_usage_json = ?, updated_at_ms = ? WHERE request_id = ?")
            .bind(status.as_str()).bind(serde_json::to_string(&usage)?).bind(now_ms()).bind(request_id)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn begin_provider_call(&self, request_id: &str, call_index: usize) -> Result<()> {
        sqlx::query(
            "UPDATE runs SET provider_call_index = ?, updated_at_ms = ? WHERE request_id = ?",
        )
        .bind(call_index as i64)
        .bind(now_ms())
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_tool_result(
        &self,
        request_id: &str,
        batch_index: usize,
        call_index: usize,
        result: &ToolResult,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO run_tool_results
             (request_id, batch_index, call_index, completion_seq, call_id, output_json, is_error, completed_at_ms)
             VALUES (?, ?, ?,
               (SELECT COALESCE(MAX(completion_seq), -1) + 1 FROM run_tool_results
                WHERE request_id = ? AND batch_index = ?),
               ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(batch_index as i64)
        .bind(call_index as i64)
        .bind(request_id)
        .bind(batch_index as i64)
        .bind(&result.call_id)
        .bind(serde_json::to_string(&result.output)?)
        .bind(result.is_error)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_tool_results(
        &self,
        request_id: &str,
        batch_index: usize,
    ) -> Result<Vec<ToolResult>> {
        let rows = sqlx::query(
            "SELECT call_id, output_json, is_error FROM run_tool_results
             WHERE request_id = ? AND batch_index = ? ORDER BY completion_seq",
        )
        .bind(request_id)
        .bind(batch_index as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ToolResult {
                    call_id: row.get(0),
                    output: serde_json::from_str(row.get::<&str, _>(1))?,
                    is_error: row.get(2),
                })
            })
            .collect()
    }

    pub async fn clear_tool_results(&self, request_id: &str, batch_index: usize) -> Result<()> {
        sqlx::query("DELETE FROM run_tool_results WHERE request_id = ? AND batch_index = ?")
            .bind(request_id)
            .bind(batch_index as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
