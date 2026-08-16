use sqlx::Row;

use crate::{model::Conversation, Result};

use super::{now_ms, BlobId, Store};

impl Store {
    pub async fn begin_revision(&self, conversation_id: &str) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        Self::ensure_conversation_tx(&mut tx, conversation_id).await?;
        sqlx::query("UPDATE conversations SET revision = revision + 1, updated_at_ms = ? WHERE conversation_id = ?")
            .bind(now_ms())
            .bind(conversation_id)
            .execute(&mut *tx)
            .await?;
        let revision =
            sqlx::query_scalar("SELECT revision FROM conversations WHERE conversation_id = ?")
                .bind(conversation_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(revision)
    }

    pub async fn revision_is_current(&self, conversation_id: &str, revision: i64) -> Result<bool> {
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM conversations WHERE conversation_id = ?")
                .bind(conversation_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(current == Some(revision))
    }

    pub async fn publish_head(
        &self,
        conversation_id: &str,
        revision: i64,
        head: &BlobId,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE conversations SET head_blob_id = ?, updated_at_ms = ? WHERE conversation_id = ? AND revision = ?",
        )
        .bind(head.as_bytes().as_slice())
        .bind(now_ms())
        .bind(conversation_id)
        .bind(revision)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(affected == 1)
    }

    pub async fn conversation(&self, conversation_id: &str) -> Result<Option<Conversation>> {
        let row = sqlx::query(
            "SELECT conversation_id, revision, head_blob_id FROM conversations WHERE conversation_id = ?",
        )
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let head: Option<Vec<u8>> = row.get(2);
            Ok(Conversation {
                conversation_id: row.get(0),
                revision: row.get(1),
                head_blob_id: head
                    .map(|bytes| BlobId::from_bytes(&bytes).map(|id| id.to_base64()))
                    .transpose()?,
            })
        })
        .transpose()
    }
}
