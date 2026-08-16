PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS conversations (
    conversation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0,
    head_blob_id BLOB,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    conversation_id TEXT NOT NULL,
    message_seq INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    role TEXT NOT NULL,
    origin TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    runtime_event_id TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, message_seq),
    UNIQUE (conversation_id, message_id),
    UNIQUE (conversation_id, runtime_event_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id)
);

CREATE INDEX IF NOT EXISTS messages_conversation_seq
ON messages(conversation_id, message_seq);

CREATE TABLE IF NOT EXISTS blobs (
    blob_id BLOB PRIMARY KEY CHECK(length(blob_id) = 32),
    data BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    CHECK(length(data) >= 0)
);

CREATE TABLE IF NOT EXISTS blob_edges (
    parent_blob_id BLOB NOT NULL,
    child_blob_id BLOB NOT NULL,
    field_name TEXT NOT NULL,
    PRIMARY KEY (parent_blob_id, child_blob_id, field_name),
    FOREIGN KEY (parent_blob_id) REFERENCES blobs(blob_id),
    FOREIGN KEY (child_blob_id) REFERENCES blobs(blob_id)
);

CREATE INDEX IF NOT EXISTS blob_edges_child ON blob_edges(child_blob_id);

CREATE TABLE IF NOT EXISTS runs (
    request_id TEXT PRIMARY KEY,
    run_id TEXT,
    conversation_id TEXT,
    revision INTEGER,
    append_seqno INTEGER NOT NULL DEFAULT -1,
    status TEXT NOT NULL,
    provider_call_index INTEGER NOT NULL DEFAULT 0,
    turn_usage_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id)
);

CREATE INDEX IF NOT EXISTS runs_conversation_status
ON runs(conversation_id, status);

CREATE TABLE IF NOT EXISTS run_tool_results (
    request_id TEXT NOT NULL,
    batch_index INTEGER NOT NULL,
    call_index INTEGER NOT NULL,
    completion_seq INTEGER NOT NULL,
    call_id TEXT NOT NULL,
    output_json TEXT NOT NULL,
    is_error INTEGER NOT NULL,
    completed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (request_id, batch_index, call_index),
    UNIQUE (request_id, call_id),
    FOREIGN KEY (request_id) REFERENCES runs(request_id)
);

CREATE TABLE IF NOT EXISTS outbox (
    outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    operation_key TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    payload BLOB NOT NULL,
    dependency_blob_ids_json TEXT NOT NULL DEFAULT '[]',
    attempts INTEGER NOT NULL DEFAULT 0,
    acked_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(request_id, operation_key),
    FOREIGN KEY (request_id) REFERENCES runs(request_id)
);

CREATE INDEX IF NOT EXISTS outbox_pending
ON outbox(request_id, acked_at_ms, outbox_id);
