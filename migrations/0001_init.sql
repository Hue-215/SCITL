-- docs/spec/rebuild/data-model.md が正。CHECK制約はテーブル作成時にすべて入れる
-- (SQLiteはALTER TABLE ADD CONSTRAINTを持たないため)。

CREATE TABLE tasks (
    id           INTEGER PRIMARY KEY,
    title        TEXT NULL,
    description  TEXT NULL,
    deadline     TEXT NULL,
    archived_at  TEXT NULL,
    deleted_at   TEXT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE task_steps (
    id           INTEGER PRIMARY KEY,
    task_id      INTEGER NOT NULL REFERENCES tasks(id),
    description  TEXT NOT NULL,
    done_at      TEXT NULL,
    deleted_at   TEXT NULL,
    order_index  INTEGER NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE TABLE messages (
    id          INTEGER PRIMARY KEY,
    task_id     INTEGER NULL REFERENCES tasks(id),
    role        TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool')),
    content     TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('normal', 'tool_execution')),
    source      TEXT NULL,
    reasoning   TEXT NULL,
    is_error    INTEGER NOT NULL DEFAULT 0,
    turn_id     TEXT NULL,
    attempt_no  INTEGER NULL,
    deleted_at  TEXT NULL,
    created_at  TEXT NOT NULL,
    CHECK (kind <> 'tool_execution' OR json_valid(content)),
    CHECK ((turn_id IS NULL) = (attempt_no IS NULL))
);

CREATE TABLE attachments (
    id             INTEGER PRIMARY KEY,
    message_id     INTEGER NOT NULL REFERENCES messages(id),
    original_name  TEXT NOT NULL,
    mime_type      TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('text', 'image', 'other')),
    size_bytes     INTEGER NOT NULL,
    content_text   TEXT NULL,
    file_hash      TEXT NULL,
    created_at     TEXT NOT NULL,
    CHECK ((content_text IS NOT NULL) <> (file_hash IS NOT NULL))
);

CREATE INDEX idx_messages_task_created ON messages(task_id, created_at);
CREATE INDEX idx_messages_turn ON messages(turn_id, attempt_no);
CREATE INDEX idx_task_steps_task_order ON task_steps(task_id, order_index);
CREATE INDEX idx_attachments_message ON attachments(message_id);
CREATE INDEX idx_attachments_file_hash ON attachments(file_hash);
