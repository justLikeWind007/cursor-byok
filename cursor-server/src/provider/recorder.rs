use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    time::Instant,
};

use crate::{
    model::{NewLlmCall, Usage},
    store::Store,
    Result,
};

use super::{FinishReason, ModelEvent};

pub(crate) fn recorded_headers(
    config: &crate::config::ProviderConfig,
    defaults: &[(&str, &str)],
) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for (name, value) in defaults {
        output.insert((*name).into(), (*value).into());
    }
    for (name, value) in &config.custom_headers {
        if crate::model::is_sensitive_header(name.as_str()) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            output.insert(name.as_str().into(), value.into());
        }
    }
    serde_json::Value::Object(output)
}

#[derive(Clone)]
pub struct CallRecorder {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    call_id: String,
    started: Instant,
    detailed: bool,
    next_chunk: AtomicI64,
    finished: AtomicBool,
}

impl CallRecorder {
    pub async fn start(store: Store, mut call: NewLlmCall) -> Result<Self> {
        call.detailed = store.detailed_logging().await?;
        store.start_llm_call(&call).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                call_id: call.call_id,
                started: Instant::now(),
                detailed: call.detailed,
                next_chunk: AtomicI64::new(0),
                finished: AtomicBool::new(false),
            }),
        })
    }

    pub fn detailed(&self) -> bool {
        self.inner.detailed
    }

    pub fn is_finished(&self) -> bool {
        self.inner.finished.load(Ordering::Acquire)
    }

    pub async fn request(
        &self,
        headers: serde_json::Value,
        body: &serde_json::Value,
    ) -> Result<()> {
        self.inner
            .store
            .record_llm_request(&self.inner.call_id, &headers, body, self.inner.detailed)
            .await?;
        Ok(())
    }

    pub async fn response_headers(&self, status: u16) -> Result<()> {
        self.inner
            .store
            .record_llm_response_headers(&self.inner.call_id, self.elapsed_ms(), status)
            .await
    }

    pub async fn response_chunk(&self, data: &[u8]) -> Result<()> {
        let seq = self.inner.next_chunk.fetch_add(1, Ordering::Relaxed);
        self.inner
            .store
            .record_llm_chunk(
                &self.inner.call_id,
                seq,
                self.elapsed_ms(),
                data,
                self.inner.detailed,
            )
            .await
    }

    pub async fn event(&self, event: &ModelEvent) -> Result<()> {
        match event {
            ModelEvent::TextDelta(_) => {
                self.inner
                    .store
                    .record_llm_first_text(&self.inner.call_id, self.elapsed_ms())
                    .await?;
            }
            ModelEvent::Usage(usage) => self.usage(*usage).await?,
            ModelEvent::Done(reason) => self.completed(*reason).await?,
            _ => {}
        }
        Ok(())
    }

    pub async fn usage(&self, usage: Usage) -> Result<()> {
        self.inner
            .store
            .record_llm_usage(&self.inner.call_id, usage)
            .await
    }

    pub async fn completed(&self, reason: FinishReason) -> Result<()> {
        self.finish("completed", Some(finish_reason(reason)), None, None)
            .await
    }

    pub async fn failed(&self, error: &crate::Error) -> Result<()> {
        self.finish(
            "error",
            None,
            Some(error_kind(error)),
            Some(&error.to_string()),
        )
        .await
    }

    pub async fn cancelled(&self) -> Result<()> {
        self.finish("cancelled", None, None, None).await
    }

    async fn finish(
        &self,
        status: &str,
        reason: Option<&str>,
        error_kind: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner
            .store
            .finish_llm_call(
                &self.inner.call_id,
                status,
                reason,
                self.elapsed_ms(),
                error_kind,
                error_message,
            )
            .await
    }

    fn elapsed_ms(&self) -> i64 {
        self.inner
            .started
            .elapsed()
            .as_millis()
            .min(i64::MAX as u128) as i64
    }
}

fn finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolUse => "tool_use",
    }
}

fn error_kind(error: &crate::Error) -> &'static str {
    match error {
        crate::Error::Provider(_) | crate::Error::Http(_) => "provider",
        crate::Error::Cancelled => "cancelled",
        crate::Error::Database(_) | crate::Error::Store(_) => "store",
        _ => "internal",
    }
}
