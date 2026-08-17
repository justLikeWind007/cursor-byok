use serde::Serialize;

use super::ProviderType;

#[derive(Clone, Debug)]
pub struct NewLlmCall {
    pub call_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub provider_call_index: i64,
    pub model_hash: String,
    pub provider_type: ProviderType,
    pub provider_url: String,
    pub model_id: String,
    pub display_name: String,
    pub message_count: usize,
    pub tool_count: usize,
    pub detailed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LlmCallSummary {
    pub call_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub provider_call_index: i64,
    pub model_hash: Option<String>,
    pub provider_type: String,
    pub provider_url: String,
    pub model_id: String,
    pub display_name: String,
    pub status: String,
    pub finish_reason: Option<String>,
    pub created_at_ms: i64,
    pub request_started_at_ms: Option<i64>,
    pub response_headers_at_ms: Option<i64>,
    pub first_event_at_ms: Option<i64>,
    pub first_text_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub queue_ms: Option<i64>,
    pub ttfb_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub usage: Option<serde_json::Value>,
    pub message_count: i64,
    pub tool_count: i64,
    pub request_bytes: Option<i64>,
    pub response_bytes: i64,
    pub stream_event_count: i64,
    pub http_status: Option<i64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub detailed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LlmCallRequest {
    pub headers: serde_json::Value,
    pub body: serde_json::Value,
    pub byte_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LlmCallResponseChunk {
    pub seq: i64,
    pub received_offset_ms: i64,
    pub data: String,
    pub byte_count: i64,
}
