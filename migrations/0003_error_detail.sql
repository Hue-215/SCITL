-- エラー発言の詳細(プロバイダーの状態コードと応答本文等)。画面の「詳細を表示」専用で、
-- モデル入力・エクスポートには使わない(docs/spec/data-model/messages.md)。
ALTER TABLE messages ADD COLUMN error_detail TEXT NULL;

-- 詳細を持てるのはエラー発言だけで、未設定はNULLに寄せる。0001のCHECKに足せないため
-- トリガーで表す(docs/spec/data-model/tables.md 5節)。
CREATE TRIGGER messages_error_detail_insert
BEFORE INSERT ON messages
WHEN NEW.error_detail IS NOT NULL AND (NEW.role <> 'error' OR NEW.error_detail = '')
BEGIN
    SELECT RAISE(ABORT, 'error_detail must be NULL or a non-empty string on an error message');
END;

CREATE TRIGGER messages_error_detail_update
BEFORE UPDATE OF role, error_detail ON messages
WHEN NEW.error_detail IS NOT NULL AND (NEW.role <> 'error' OR NEW.error_detail = '')
BEGIN
    SELECT RAISE(ABORT, 'error_detail must be NULL or a non-empty string on an error message');
END;
