use sqlx::{Row, Sqlite, Transaction};

use crate::{
    model::{Conversation, ConversationId, RevisionId, RunId},
    Error, Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn conversation(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<Option<Conversation>> {
        let row = sqlx::query(
            "SELECT current_revision_id, active_run_id
             FROM conversations WHERE conversation_id = ?",
        )
        .bind(conversation_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| Conversation {
            conversation_id: conversation_id.clone(),
            current_revision_id: RevisionId(row.get(0)),
            active_run_id: row.get::<Option<String>, _>(1).map(RunId),
        }))
    }

    pub(crate) async fn ensure_conversation_tx(
        tx: &mut Transaction<'_, Sqlite>,
        conversation_id: &ConversationId,
    ) -> Result<RevisionId> {
        sqlx::query(
            "INSERT OR IGNORE INTO conversations(conversation_id, updated_at_ms) VALUES (?, ?)",
        )
        .bind(conversation_id.as_str())
        .bind(now_ms())
        .execute(&mut **tx)
        .await?;

        let current: Option<i64> = sqlx::query_scalar(
            "SELECT current_revision_id FROM conversations WHERE conversation_id = ?",
        )
        .bind(conversation_id.as_str())
        .fetch_one(&mut **tx)
        .await?;
        if let Some(current) = current {
            return Ok(RevisionId(current));
        }

        let digest = super::revisions::message_digest(&[])?;
        let root = sqlx::query(
            "INSERT INTO conversation_revisions
             (conversation_id, parent_revision_id, state_digest, created_at_ms)
             VALUES (?, NULL, ?, ?)",
        )
        .bind(conversation_id.as_str())
        .bind(digest.as_slice())
        .bind(now_ms())
        .execute(&mut **tx)
        .await?
        .last_insert_rowid();
        sqlx::query(
            "UPDATE conversations SET current_revision_id = ?, updated_at_ms = ?
             WHERE conversation_id = ? AND current_revision_id IS NULL",
        )
        .bind(root)
        .bind(now_ms())
        .bind(conversation_id.as_str())
        .execute(&mut **tx)
        .await?;
        Ok(RevisionId(root))
    }

    pub(crate) async fn require_active_head_tx(
        tx: &mut Transaction<'_, Sqlite>,
        conversation_id: &ConversationId,
        run_id: &RunId,
        expected: RevisionId,
    ) -> Result<()> {
        let row = sqlx::query(
            "SELECT current_revision_id, active_run_id FROM conversations WHERE conversation_id = ?",
        )
        .bind(conversation_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(row)
                if row.get::<Option<i64>, _>(0) == Some(expected.0)
                    && row.get::<Option<&str>, _>(1) == Some(run_id.as_str()) =>
            {
                Ok(())
            }
            _ => Err(Error::Store(format!(
                "run {run_id} no longer owns conversation {conversation_id} at revision {expected}"
            ))),
        }
    }
}
