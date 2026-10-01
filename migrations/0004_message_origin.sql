-- 行の3分類(docs/spec/data-model/messages.md「ターン境界」)。経路の印(source)を持つのは、
-- 応答生成以外の経路での操作の記録(ターンに属さないツール実行記録)だけ。ターンの行に
-- 印を付けたり、印の無い記録をターンの外に書いたりすると、表示と履歴の見分けが崩れる。
-- 0001のCHECKに足せないためトリガーで表す(docs/spec/data-model/tables.md「マイグレーション」)。
CREATE TRIGGER messages_origin_insert
BEFORE INSERT ON messages
WHEN (NEW.source IS NOT NULL) <> (NEW.kind = 'tool_execution' AND NEW.turn_id IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'source is set if and only if the row is an operation record outside a turn');
END;

CREATE TRIGGER messages_origin_update
BEFORE UPDATE OF source, kind, turn_id ON messages
WHEN (NEW.source IS NOT NULL) <> (NEW.kind = 'tool_execution' AND NEW.turn_id IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'source is set if and only if the row is an operation record outside a turn');
END;
