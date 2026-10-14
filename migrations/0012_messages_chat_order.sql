-- 会話の行の並びは`id`の順だけにする(docs/spec/data-model/messages.md「ターン境界」)。会話ごとの取得を
-- 並べ替えずに済むよう、索引も`created_at`から`id`に替える。
DROP INDEX idx_messages_task_created;
CREATE INDEX idx_messages_task_id ON messages(task_id, id);
