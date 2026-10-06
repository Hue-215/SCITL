-- ターンの返信の行(通常発言・エラー発言)が、そのターンの中身を起きた順の配列で持つ
-- (docs/spec/data-model/messages.md「1ターン内の往復で保存するもの」)。要素は本文・思考・
-- ツールで、ツールは実行記録の行をidで指す。
ALTER TABLE messages ADD COLUMN parts TEXT NULL;

-- 既存の返信を配列へ移す。実行記録はid順に、記録ごとに1ラウンドとする(ラウンドの区切りは
-- 記録に無い。今の実行記録からの履歴の組み立てと同じ発言列になる)。記録の思考はその記録の
-- 前に置く。最後のラウンドには、返信の行の思考と本文(アシスタント発言は`content`、エラー発言は
-- 受け取り終えた本文`partial_reply`)を置く。消した記録も指す(新しく書く中身と同じく、記録の
-- 側の`deleted_at`を戻せば返信の中身にも戻る。読むときに消した記録は飛ばす)。消した記録の思考は
-- 移さない(読むときに飛ばせないため。今までも表示されていない)。空白だけの思考と本文は、Rust側と
-- 同じく改行・タブも空白として外す。
--
-- 試行ごとの記録は索引を張った一時表に置いてから引く(長く使った会話でも行数に比例する時間で済む)。
CREATE TEMP TABLE migrated_records AS
SELECT id, turn_id, attempt_no, reasoning, deleted_at,
       ROW_NUMBER() OVER (PARTITION BY turn_id, attempt_no ORDER BY id) AS round
FROM messages
WHERE kind = 'tool_execution' AND turn_id IS NOT NULL;

CREATE INDEX temp.migrated_records_attempt ON migrated_records (turn_id, attempt_no);

CREATE TEMP TABLE migrated_parts AS
WITH counts AS (
    SELECT turn_id, attempt_no, COUNT(*) AS n
    FROM migrated_records
    GROUP BY turn_id, attempt_no
),
replies AS (
    SELECT m.id, m.turn_id, m.attempt_no, m.reasoning,
           CASE m.role WHEN 'assistant' THEN m.content ELSE m.partial_reply END AS text,
           coalesce(c.n, 0) + 1 AS round
    FROM messages AS m
    LEFT JOIN counts AS c ON c.turn_id = m.turn_id AND c.attempt_no = m.attempt_no
    WHERE m.kind = 'normal' AND m.turn_id IS NOT NULL
),
parts (reply_id, round, seq, part) AS (
    SELECT p.id, r.round, 0,
           json_object('type', 'reasoning', 'round', r.round, 'text', r.reasoning)
    FROM replies AS p
    JOIN migrated_records AS r ON r.turn_id = p.turn_id AND r.attempt_no = p.attempt_no
    WHERE trim(coalesce(r.reasoning, ''), ' ' || char(9, 10, 13)) <> '' AND r.deleted_at IS NULL
    UNION ALL
    SELECT p.id, r.round, 1, json_object('type', 'tool', 'round', r.round, 'record', r.id)
    FROM replies AS p
    JOIN migrated_records AS r ON r.turn_id = p.turn_id AND r.attempt_no = p.attempt_no
    UNION ALL
    SELECT id, round, 0, json_object('type', 'reasoning', 'round', round, 'text', reasoning)
    FROM replies
    WHERE trim(coalesce(reasoning, ''), ' ' || char(9, 10, 13)) <> ''
    UNION ALL
    SELECT id, round, 1, json_object('type', 'text', 'round', round, 'text', text)
    FROM replies
    WHERE trim(coalesce(text, ''), ' ' || char(9, 10, 13)) <> ''
)
SELECT reply_id, json_group_array(json(part) ORDER BY round, seq) AS parts
FROM parts
GROUP BY reply_id;

CREATE INDEX temp.migrated_parts_reply ON migrated_parts (reply_id);

UPDATE messages
SET parts = coalesce((SELECT parts FROM migrated_parts WHERE reply_id = messages.id), '[]')
WHERE kind = 'normal' AND turn_id IS NOT NULL;

DROP TABLE migrated_parts;
DROP TABLE migrated_records;

-- 本文は配列だけに持つ。
UPDATE messages SET content = ''
WHERE kind = 'normal' AND role = 'assistant' AND turn_id IS NOT NULL;

-- 配列へ移した列を外す。列を参照するトリガーが残っていると外せない。
DROP TRIGGER messages_partial_reply_insert;
DROP TRIGGER messages_partial_reply_update;
ALTER TABLE messages DROP COLUMN partial_reply;
ALTER TABLE messages DROP COLUMN reasoning;

-- 配列を持つのはターンの返信の行(`turn_id`を持つ通常発言)だけで、それには必ず持つ。中身は
-- JSONの配列。0001のCHECKに足せないためトリガーで表す(docs/spec/data-model/tables.md「マイグレーション」)。
CREATE TRIGGER messages_parts_insert
BEFORE INSERT ON messages
WHEN (NEW.parts IS NOT NULL) <> (NEW.kind = 'normal' AND NEW.turn_id IS NOT NULL)
    OR (NEW.parts IS NOT NULL AND (NOT json_valid(NEW.parts) OR json_type(NEW.parts) <> 'array'))
BEGIN
    SELECT RAISE(ABORT, 'parts is a JSON array set if and only if the row is the reply of a turn');
END;

CREATE TRIGGER messages_parts_update
BEFORE UPDATE OF parts, kind, turn_id ON messages
WHEN (NEW.parts IS NOT NULL) <> (NEW.kind = 'normal' AND NEW.turn_id IS NOT NULL)
    OR (NEW.parts IS NOT NULL AND (NOT json_valid(NEW.parts) OR json_type(NEW.parts) <> 'array'))
BEGIN
    SELECT RAISE(ABORT, 'parts is a JSON array set if and only if the row is the reply of a turn');
END;
