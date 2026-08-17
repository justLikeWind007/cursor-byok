use crate::{
    model::{ConversationId, RevisionId},
    Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn anchor_input(
        &self,
        conversation_id: &ConversationId,
        input_id: &str,
        base_revision_id: RevisionId,
    ) -> Result<RevisionId> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO input_anchors
             (conversation_id, input_id, base_revision_id, created_at_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(conversation_id, input_id) DO NOTHING",
        )
        .bind(conversation_id.as_str())
        .bind(input_id)
        .bind(base_revision_id.0)
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
        let anchored = sqlx::query_scalar::<_, i64>(
            "SELECT base_revision_id FROM input_anchors
             WHERE conversation_id = ? AND input_id = ?",
        )
        .bind(conversation_id.as_str())
        .bind(input_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RevisionId(anchored))
    }
}
