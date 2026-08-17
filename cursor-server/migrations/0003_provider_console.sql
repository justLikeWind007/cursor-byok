CREATE TABLE provider_endpoints (
    provider_id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    custom_headers_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE provider_models (
    model_hash TEXT PRIMARY KEY CHECK(length(model_hash) = 8),
    provider_id INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0,
    context_window_tokens INTEGER,
    max_output_tokens INTEGER,
    reasoning_enabled INTEGER NOT NULL DEFAULT 0,
    reasoning_effort TEXT,
    extra_params_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(provider_id, model_id),
    FOREIGN KEY(provider_id) REFERENCES provider_endpoints(provider_id) ON DELETE CASCADE
);

CREATE INDEX provider_models_enabled_sort
ON provider_models(enabled, sort_order, display_name);

CREATE TABLE service_settings (
    setting_key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

INSERT INTO service_settings(setting_key, value_json, updated_at_ms)
VALUES ('llm_detailed_logging', 'false', unixepoch('subsec') * 1000);

CREATE TABLE llm_calls (
    call_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    provider_call_index INTEGER NOT NULL,
    model_hash TEXT,
    provider_type TEXT NOT NULL,
    provider_url TEXT NOT NULL,
    model_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status TEXT NOT NULL,
    finish_reason TEXT,
    created_at_ms INTEGER NOT NULL,
    request_started_at_ms INTEGER,
    response_headers_at_ms INTEGER,
    first_event_at_ms INTEGER,
    first_text_at_ms INTEGER,
    finished_at_ms INTEGER,
    queue_ms INTEGER,
    ttfb_ms INTEGER,
    ttft_ms INTEGER,
    duration_ms INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    total_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    usage_json TEXT,
    message_count INTEGER NOT NULL,
    tool_count INTEGER NOT NULL,
    request_bytes INTEGER,
    response_bytes INTEGER NOT NULL DEFAULT 0,
    stream_event_count INTEGER NOT NULL DEFAULT 0,
    http_status INTEGER,
    error_kind TEXT,
    error_message TEXT,
    detailed INTEGER NOT NULL,
    FOREIGN KEY(model_hash) REFERENCES provider_models(model_hash)
);

CREATE INDEX llm_calls_created ON llm_calls(created_at_ms DESC);
CREATE INDEX llm_calls_run ON llm_calls(run_id, provider_call_index);
CREATE INDEX llm_calls_model ON llm_calls(model_hash, created_at_ms DESC);

CREATE TABLE llm_call_requests (
    call_id TEXT PRIMARY KEY,
    headers_json TEXT NOT NULL,
    body_json TEXT NOT NULL,
    byte_count INTEGER NOT NULL,
    FOREIGN KEY(call_id) REFERENCES llm_calls(call_id) ON DELETE CASCADE
);

CREATE TABLE llm_call_response_chunks (
    call_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    received_offset_ms INTEGER NOT NULL,
    data BLOB NOT NULL,
    byte_count INTEGER NOT NULL,
    PRIMARY KEY(call_id, seq),
    FOREIGN KEY(call_id) REFERENCES llm_calls(call_id) ON DELETE CASCADE
);
