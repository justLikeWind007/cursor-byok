use sqlx::{Row, Sqlite, Transaction};

use crate::{
    model::{CanonicalMessage, RuntimeEvent},
    Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn load_messages(&self, conversation_id: &str) -> Result<Vec<CanonicalMessage>> {
        let rows = sqlx::query(
            "SELECT payload_json FROM messages WHERE conversation_id = ? ORDER BY message_seq",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get::<&str, _>(0)).map_err(Into::into))
            .collect()
    }

    pub async fn append_messages(
        &self,
        conversation_id: &str,
        messages: &[CanonicalMessage],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        Self::ensure_conversation_tx(&mut tx, conversation_id).await?;
        for message in messages {
            Self::append_message_tx(&mut tx, conversation_id, message).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn append_runtime_event_once(
        &self,
        conversation_id: &str,
        event: RuntimeEvent,
    ) -> Result<bool> {
        let message = event.into_message();
        let mut tx = self.pool.begin().await?;
        Self::ensure_conversation_tx(&mut tx, conversation_id).await?;
        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(message_seq), -1) + 1 FROM messages WHERE conversation_id = ?",
        )
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO messages
             (conversation_id, message_seq, message_id, role, origin, payload_json, runtime_event_id, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(conversation_id)
        .bind(next_seq)
        .bind(&message.message_id)
        .bind(role_name(&message.role))
        .bind(origin_name(&message.origin))
        .bind(serde_json::to_string(&message)?)
        .bind(&message.runtime_event_id)
        .bind(now_ms())
        .execute(&mut *tx)
        .await?
        .rows_affected() == 1;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn append_message_tx(
        tx: &mut Transaction<'_, Sqlite>,
        conversation_id: &str,
        message: &CanonicalMessage,
    ) -> Result<()> {
        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(message_seq), -1) + 1 FROM messages WHERE conversation_id = ?",
        )
        .bind(conversation_id)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO messages
             (conversation_id, message_seq, message_id, role, origin, payload_json, runtime_event_id, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(conversation_id)
        .bind(next_seq)
        .bind(&message.message_id)
        .bind(role_name(&message.role))
        .bind(origin_name(&message.origin))
        .bind(serde_json::to_string(message)?)
        .bind(&message.runtime_event_id)
        .bind(now_ms())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub(crate) async fn ensure_conversation_tx(
        tx: &mut Transaction<'_, Sqlite>,
        conversation_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO conversations(conversation_id, revision, updated_at_ms) VALUES (?, 0, ?)",
        )
        .bind(conversation_id)
        .bind(now_ms())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

fn role_name(role: &crate::model::Role) -> &'static str {
    match role {
        crate::model::Role::System => "system",
        crate::model::Role::User => "user",
        crate::model::Role::Assistant => "assistant",
        crate::model::Role::Tool => "tool",
    }
}

fn origin_name(origin: &crate::model::Origin) -> &'static str {
    match origin {
        crate::model::Origin::Prompt => "prompt",
        crate::model::Origin::User => "user",
        crate::model::Origin::Runtime => "runtime",
        crate::model::Origin::Assistant => "assistant",
        crate::model::Origin::Tool => "tool",
    }
}
