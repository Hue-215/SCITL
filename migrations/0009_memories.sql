-- 会話をまたいで共有する、利用者についての事実(docs/spec/data-model/tables.md memories)。
-- 1行が1つの事実で、どの会話にも属さない。物理削除せず、deleted_atで消す。
CREATE TABLE memories (
    id          INTEGER PRIMARY KEY,
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    deleted_at  TEXT NULL
);
