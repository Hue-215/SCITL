-- ツール実行記録の行は role='tool' に揃える(docs/spec/rebuild/data-model.md messages)。
UPDATE messages SET role = 'tool' WHERE kind = 'tool_execution';

-- (role = 'tool') = (kind = 'tool_execution') を強制する。SQLiteは既存テーブルへ
-- CHECKを後から足せないため、同じ条件をトリガーで表す。
CREATE TRIGGER messages_tool_role_insert
BEFORE INSERT ON messages
WHEN (NEW.role = 'tool') <> (NEW.kind = 'tool_execution')
BEGIN
    SELECT RAISE(ABORT, 'role=tool if and only if kind=tool_execution');
END;

CREATE TRIGGER messages_tool_role_update
BEFORE UPDATE OF role, kind ON messages
WHEN (NEW.role = 'tool') <> (NEW.kind = 'tool_execution')
BEGIN
    SELECT RAISE(ABORT, 'role=tool if and only if kind=tool_execution');
END;
