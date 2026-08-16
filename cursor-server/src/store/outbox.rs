use sqlx::Row;

use crate::Result;

use super::{now_ms, BlobId, Store};

#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub id: i64,
    pub request_id: String,
    pub key: String,
    pub kind: String,
    pub payload: Vec<u8>,
    pub dependencies: Vec<String>,
    pub attempts: i64,
}

impl Store {
    pub async fn enqueue_outbox(
        &self,
        request_id: &str,
        key: &str,
        kind: &str,
        payload: &[u8],
        dependencies: &[BlobId],
    ) -> Result<()> {
        let now = now_ms();
        let dependency_json = serde_json::to_string(
            &dependencies
                .iter()
                .map(BlobId::to_base64)
                .collect::<Vec<_>>(),
        )?;
        sqlx::query(
            "INSERT OR IGNORE INTO outbox
             (request_id, operation_key, operation_kind, payload, dependency_blob_ids_json, created_at_ms, updated_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id).bind(key).bind(kind).bind(payload).bind(dependency_json).bind(now).bind(now)
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn pending_outbox(&self, request_id: &str) -> Result<Vec<OutboxItem>> {
        let rows = sqlx::query(
            "SELECT outbox_id, request_id, operation_key, operation_kind, payload,
                    dependency_blob_ids_json, attempts
             FROM outbox WHERE request_id = ? AND acked_at_ms IS NULL ORDER BY outbox_id",
        )
        .bind(request_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(OutboxItem {
                    id: row.get(0),
                    request_id: row.get(1),
                    key: row.get(2),
                    kind: row.get(3),
                    payload: row.get(4),
                    dependencies: serde_json::from_str(row.get::<&str, _>(5))?,
                    attempts: row.get(6),
                })
            })
            .collect()
    }

    pub async fn mark_outbox_sent(&self, id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE outbox SET attempts = attempts + 1, updated_at_ms = ? WHERE outbox_id = ?",
        )
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn ack_outbox(&self, request_id: &str, key: &str) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE outbox SET acked_at_ms = COALESCE(acked_at_ms, ?), updated_at_ms = ?
             WHERE request_id = ? AND operation_key = ?",
        )
        .bind(now_ms())
        .bind(now_ms())
        .bind(request_id)
        .bind(key)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn dependencies_acked(
        &self,
        request_id: &str,
        dependencies: &[BlobId],
    ) -> Result<bool> {
        for id in dependencies {
            let key = format!("blob:{}", id.to_base64());
            let acked: Option<i64> = sqlx::query_scalar(
                "SELECT acked_at_ms FROM outbox WHERE request_id = ? AND operation_key = ?",
            )
            .bind(request_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
            if acked.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
