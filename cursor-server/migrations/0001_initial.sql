PRAGMA foreign_keys = ON;

CREATE TABLE conversations (
    conversation_id TEXT PRIMARY KEY,
    current_revision_id INTEGER,
    active_run_id TEXT,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE messages (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    role TEXT NOT NULL,
    origin TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    runtime_event_id TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, message_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id)
);

CREATE UNIQUE INDEX messages_runtime_event
ON messages(conversation_id, runtime_event_id)
WHERE runtime_event_id IS NOT NULL;

CREATE TABLE conversation_revisions (
    revision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    parent_revision_id INTEGER,
    state_digest BLOB NOT NULL CHECK(length(state_digest) = 32),
    created_at_ms INTEGER NOT NULL,
    UNIQUE (conversation_id, state_digest),
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id),
    FOREIGN KEY (parent_revision_id) REFERENCES conversation_revisions(revision_id)
);

CREATE INDEX conversation_revisions_parent
ON conversation_revisions(conversation_id, parent_revision_id);

CREATE TABLE revision_messages (
    revision_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (revision_id, ordinal),
    UNIQUE (revision_id, message_id),
    FOREIGN KEY (revision_id) REFERENCES conversation_revisions(revision_id),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages(conversation_id, message_id)
);

CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    base_revision_id INTEGER NOT NULL,
    head_revision_id INTEGER NOT NULL,
    parent_run_id TEXT,
    parent_tool_call_id TEXT,
    run_kind TEXT NOT NULL,
    subagent_kind TEXT,
    status TEXT NOT NULL,
    provider_call_index INTEGER NOT NULL DEFAULT -1,
    turn_usage_json TEXT NOT NULL DEFAULT 'null',
    failure_category TEXT,
    failure_summary TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id),
    FOREIGN KEY (base_revision_id) REFERENCES conversation_revisions(revision_id),
    FOREIGN KEY (head_revision_id) REFERENCES conversation_revisions(revision_id)
);

CREATE INDEX runs_conversation_status
ON runs(conversation_id, status);

CREATE TABLE tool_rounds (
    round_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    base_revision_id INTEGER NOT NULL,
    assistant_json TEXT NOT NULL,
    status TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    next_completion_seq INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs(run_id),
    FOREIGN KEY (base_revision_id) REFERENCES conversation_revisions(revision_id)
);

CREATE INDEX tool_rounds_run_status
ON tool_rounds(run_id, status);

CREATE TABLE tool_round_calls (
    round_id TEXT NOT NULL,
    call_index INTEGER NOT NULL,
    call_id TEXT NOT NULL,
    model_call_id TEXT NOT NULL,
    name TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    status TEXT NOT NULL,
    completion_seq INTEGER,
    result_content TEXT,
    result_is_error INTEGER,
    committed_revision_id INTEGER,
    completed_at_ms INTEGER,
    PRIMARY KEY (round_id, call_index),
    UNIQUE (round_id, call_id),
    UNIQUE (round_id, completion_seq),
    FOREIGN KEY (round_id) REFERENCES tool_rounds(round_id),
    FOREIGN KEY (committed_revision_id) REFERENCES conversation_revisions(revision_id)
);

CREATE TABLE blobs (
    blob_id BLOB PRIMARY KEY CHECK(length(blob_id) = 32),
    data BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE blob_edges (
    parent_blob_id BLOB NOT NULL,
    child_blob_id BLOB NOT NULL,
    field_name TEXT NOT NULL,
    PRIMARY KEY (parent_blob_id, child_blob_id, field_name),
    FOREIGN KEY (parent_blob_id) REFERENCES blobs(blob_id),
    FOREIGN KEY (child_blob_id) REFERENCES blobs(blob_id)
);

CREATE INDEX blob_edges_child ON blob_edges(child_blob_id);
