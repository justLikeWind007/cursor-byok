CREATE TABLE input_anchors (
    conversation_id TEXT NOT NULL,
    input_id TEXT NOT NULL,
    base_revision_id INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, input_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id),
    FOREIGN KEY (base_revision_id) REFERENCES conversation_revisions(revision_id)
);
