use std::str::FromStr;

use sqlx::Row;

use crate::{
    model::{
        model_hash, normalize_base_url, ProviderEndpoint, ProviderEndpointInput,
        ProviderEndpointSecret, ProviderModel, ProviderModelInput, ProviderType,
    },
    Error, Result,
};

use super::{now_ms, Store};

impl Store {
    pub async fn providers(&self) -> Result<Vec<ProviderEndpoint>> {
        let rows = sqlx::query(
            "SELECT provider_id, name, provider_type, base_url, api_key, custom_headers_json, created_at_ms, updated_at_ms FROM provider_endpoints ORDER BY provider_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(endpoint_from_row).collect()
    }

    pub async fn provider(&self, provider_id: i64) -> Result<Option<ProviderEndpointSecret>> {
        let row = sqlx::query(
            "SELECT provider_id, name, provider_type, base_url, api_key, custom_headers_json, created_at_ms, updated_at_ms FROM provider_endpoints WHERE provider_id = ?",
        )
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(secret_from_row).transpose()
    }

    pub async fn create_provider(&self, input: &ProviderEndpointInput) -> Result<ProviderEndpoint> {
        validate_endpoint(input)?;
        let now = now_ms();
        let base_url = normalize_base_url(&input.base_url)?;
        let result = sqlx::query(
            "INSERT INTO provider_endpoints(name, provider_type, base_url, api_key, custom_headers_json, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.name.trim())
        .bind(input.provider_type.as_str())
        .bind(base_url)
        .bind(input.api_key.as_deref().unwrap_or_default())
        .bind(serde_json::to_string(&input.custom_headers)?)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(self
            .provider(result.last_insert_rowid())
            .await?
            .expect("inserted provider must exist")
            .endpoint)
    }

    pub async fn update_provider(
        &self,
        provider_id: i64,
        input: &ProviderEndpointInput,
    ) -> Result<ProviderEndpoint> {
        validate_endpoint(input)?;
        let current = self
            .provider(provider_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("provider {provider_id}")))?;
        let api_key = input.api_key.as_deref().unwrap_or(&current.api_key);
        let custom_headers = merge_custom_headers(&current.custom_headers, &input.custom_headers)?;
        let base_url = normalize_base_url(&input.base_url)?;
        let identity_changed = base_url != current.endpoint.base_url
            || input.provider_type != current.endpoint.provider_type;
        if identity_changed
            && sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM provider_models WHERE provider_id = ?",
            )
            .bind(provider_id)
            .fetch_one(&self.pool)
            .await?
                > 0
        {
            return Err(Error::Config(
                "provider URL and type are immutable after models are added; create a new provider"
                    .into(),
            ));
        }
        sqlx::query(
            "UPDATE provider_endpoints SET name = ?, provider_type = ?, base_url = ?, api_key = ?, custom_headers_json = ?, updated_at_ms = ? WHERE provider_id = ?",
        )
        .bind(input.name.trim())
        .bind(input.provider_type.as_str())
        .bind(&base_url)
        .bind(api_key)
        .bind(serde_json::to_string(&custom_headers)?)
        .bind(now_ms())
        .bind(provider_id)
        .execute(&self.pool)
        .await?;
        Ok(self.provider(provider_id).await?.unwrap().endpoint)
    }

    pub async fn delete_provider(&self, provider_id: i64) -> Result<()> {
        let result = sqlx::query("DELETE FROM provider_endpoints WHERE provider_id = ?")
            .bind(provider_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(Error::RunNotFound(format!("provider {provider_id}")));
        }
        Ok(())
    }

    pub async fn provider_models(&self, enabled_only: bool) -> Result<Vec<ProviderModel>> {
        let query = if enabled_only {
            "SELECT * FROM provider_models WHERE enabled = 1 ORDER BY sort_order, display_name"
        } else {
            "SELECT * FROM provider_models ORDER BY sort_order, display_name"
        };
        let rows = sqlx::query(query).fetch_all(&self.pool).await?;
        rows.into_iter().map(model_from_row).collect()
    }

    pub async fn provider_model(&self, hash: &str) -> Result<Option<ProviderModel>> {
        sqlx::query("SELECT * FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?
            .map(model_from_row)
            .transpose()
    }

    pub async fn save_provider_model(
        &self,
        provider_id: i64,
        input: &ProviderModelInput,
    ) -> Result<ProviderModel> {
        if input.display_name.trim().is_empty() {
            return Err(Error::Config("model display name cannot be empty".into()));
        }
        let provider = self
            .provider(provider_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("provider {provider_id}")))?;
        let hash = model_hash(
            &provider.endpoint.base_url,
            provider.endpoint.provider_type,
            &input.model_id,
        )?;
        assert_hash_available(&self.pool, &hash, provider_id, input.model_id.trim()).await?;
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO provider_models(
                model_hash, provider_id, model_id, display_name, enabled, sort_order,
                context_window_tokens, max_output_tokens, reasoning_enabled,
                reasoning_effort, extra_params_json, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(provider_id, model_id) DO UPDATE SET
                display_name = excluded.display_name,
                enabled = excluded.enabled,
                sort_order = excluded.sort_order,
                context_window_tokens = excluded.context_window_tokens,
                max_output_tokens = excluded.max_output_tokens,
                reasoning_enabled = excluded.reasoning_enabled,
                reasoning_effort = excluded.reasoning_effort,
                extra_params_json = excluded.extra_params_json,
                updated_at_ms = excluded.updated_at_ms"#,
        )
        .bind(&hash)
        .bind(provider_id)
        .bind(input.model_id.trim())
        .bind(input.display_name.trim())
        .bind(input.enabled)
        .bind(input.sort_order)
        .bind(input.context_window_tokens.map(|value| value as i64))
        .bind(input.max_output_tokens.map(|value| value as i64))
        .bind(input.reasoning_enabled)
        .bind(&input.reasoning_effort)
        .bind(serde_json::to_string(&input.extra_params)?)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(self.provider_model(&hash).await?.unwrap())
    }

    pub async fn delete_provider_model(&self, hash: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(Error::RunNotFound(format!("model {hash}")));
        }
        Ok(())
    }
}

fn validate_endpoint(input: &ProviderEndpointInput) -> Result<()> {
    if input.name.trim().is_empty() {
        return Err(Error::Config("provider name cannot be empty".into()));
    }
    if !input.custom_headers.is_object() {
        return Err(Error::Config("custom headers must be a JSON object".into()));
    }
    normalize_base_url(&input.base_url)?;
    Ok(())
}

fn endpoint_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderEndpoint> {
    let api_key: String = row.try_get("api_key")?;
    let headers: serde_json::Value = serde_json::from_str(row.try_get("custom_headers_json")?)?;
    Ok(ProviderEndpoint {
        provider_id: row.try_get("provider_id")?,
        name: row.try_get("name")?,
        provider_type: ProviderType::from_str(row.try_get("provider_type")?)?,
        base_url: row.try_get("base_url")?,
        has_api_key: !api_key.is_empty(),
        custom_headers: redact_custom_headers(&headers),
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn secret_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderEndpointSecret> {
    let api_key: String = row.try_get("api_key")?;
    let custom_headers: serde_json::Value =
        serde_json::from_str(row.try_get("custom_headers_json")?)?;
    Ok(ProviderEndpointSecret {
        endpoint: endpoint_from_row(row)?,
        api_key,
        custom_headers,
    })
}

fn redact_custom_headers(headers: &serde_json::Value) -> serde_json::Value {
    let mut headers = headers.clone();
    if let Some(object) = headers.as_object_mut() {
        for (name, value) in object {
            if crate::model::is_sensitive_header(name) {
                *value = serde_json::Value::Null;
            }
        }
    }
    headers
}

fn merge_custom_headers(
    current: &serde_json::Value,
    update: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut output = update
        .as_object()
        .ok_or_else(|| Error::Config("custom headers must be a JSON object".into()))?
        .clone();
    let current = current
        .as_object()
        .expect("stored custom headers are validated");
    for (name, value) in &mut output {
        if value.is_null() {
            *value = current.get(name).cloned().ok_or_else(|| {
                Error::Config(format!(
                    "custom header {name} has no existing value to retain"
                ))
            })?;
        }
    }
    Ok(serde_json::Value::Object(output))
}

fn model_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProviderModel> {
    Ok(ProviderModel {
        model_hash: row.try_get("model_hash")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        display_name: row.try_get("display_name")?,
        enabled: row.try_get("enabled")?,
        sort_order: row.try_get("sort_order")?,
        context_window_tokens: row
            .try_get::<Option<i64>, _>("context_window_tokens")?
            .map(|value| value as u64),
        max_output_tokens: row
            .try_get::<Option<i64>, _>("max_output_tokens")?
            .map(|value| value as u64),
        reasoning_enabled: row.try_get("reasoning_enabled")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        extra_params: serde_json::from_str(row.try_get("extra_params_json")?)?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

async fn assert_hash_available(
    pool: &sqlx::SqlitePool,
    hash: &str,
    provider_id: i64,
    model_id: &str,
) -> Result<()> {
    let existing =
        sqlx::query("SELECT provider_id, model_id FROM provider_models WHERE model_hash = ?")
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    if let Some(row) = existing {
        if row.try_get::<i64, _>("provider_id")? != provider_id
            || row.try_get::<String, _>("model_id")? != model_id
        {
            return Err(Error::Config(format!(
                "8-character model hash collision: {hash}"
            )));
        }
    }
    Ok(())
}
