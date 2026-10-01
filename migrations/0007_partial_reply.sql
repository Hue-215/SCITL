-- 失敗したターンで、受け取り終えたラウンドの本文(docs/spec/rebuild/data-model.md messages)。
-- 表示とエクスポートのためだけに持ち、モデルには送らない。
ALTER TABLE messages ADD COLUMN partial_reply TEXT NULL;

-- 持てるのはエラー発言だけで、未設定はNULLに寄せる。0001のCHECKに足せないためトリガーで表す
-- (data-model.md 5節)。
CREATE TRIGGER messages_partial_reply_insert
BEFORE INSERT ON messages
WHEN NEW.partial_reply IS NOT NULL AND (NEW.role <> 'error' OR NEW.partial_reply = '')
BEGIN
    SELECT RAISE(ABORT, 'partial_reply must be NULL or a non-empty string on an error message');
END;

CREATE TRIGGER messages_partial_reply_update
BEFORE UPDATE OF role, partial_reply ON messages
WHEN NEW.partial_reply IS NOT NULL AND (NEW.role <> 'error' OR NEW.partial_reply = '')
BEGIN
    SELECT RAISE(ABORT, 'partial_reply must be NULL or a non-empty string on an error message');
END;
