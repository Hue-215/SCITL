-- 同期のために持つ情報(docs/sync/index.md「構成」。列の意味はdocs/spec/data-model/tables.md「同期のための列」)。
-- 編集する側のコードは同期を知らず、`uid`・`sync_seq`・印はここのトリガーが付ける。
--
-- 足す列の制約は`ADD COLUMN`に書く(既存の行にも確かめられる)。`uid`は既定値に式を置けないので
-- NULL可とし、INSERTのトリガーが振る。

-- DBに1つの端末IDとカウンター。`sync_seq`と版の番号はどちらもこのカウンターから取る。
CREATE TABLE sync_state (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    device_id  TEXT NOT NULL
               CHECK (typeof(device_id) = 'text' AND length(device_id) = 32
                      AND device_id NOT GLOB '*[^0-9a-f]*'),
    counter    INTEGER NOT NULL CHECK (counter >= 1)
);

-- 既存の行は番号1で書いたものとする(カーソルを0にした同期で、すべて送られる)。
INSERT INTO sync_state (id, device_id, counter) VALUES (1, lower(hex(randomblob(16))), 1);

-- 相手の端末IDごとのカーソル。`sent_seq`は相手へ渡した位置(自分の番号)、`received_seq`は相手から
-- 受け取った位置(相手の番号)。
CREATE TABLE sync_cursors (
    peer_device_id  TEXT NOT NULL PRIMARY KEY
                    CHECK (typeof(peer_device_id) = 'text' AND length(peer_device_id) = 32
                           AND peer_device_id NOT GLOB '*[^0-9a-f]*'),
    sent_seq        INTEGER NOT NULL DEFAULT 0 CHECK (sent_seq >= 0),
    received_seq    INTEGER NOT NULL DEFAULT 0 CHECK (received_seq >= 0)
);

ALTER TABLE tasks ADD COLUMN uid TEXT NULL
    CHECK (uid IS NULL OR (typeof(uid) = 'text' AND length(uid) = 32 AND uid NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE tasks ADD COLUMN sync_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE task_steps ADD COLUMN uid TEXT NULL
    CHECK (uid IS NULL OR (typeof(uid) = 'text' AND length(uid) = 32 AND uid NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE task_steps ADD COLUMN sync_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN uid TEXT NULL
    CHECK (uid IS NULL OR (typeof(uid) = 'text' AND length(uid) = 32 AND uid NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE memories ADD COLUMN sync_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN uid TEXT NULL
    CHECK (uid IS NULL OR (typeof(uid) = 'text' AND length(uid) = 32 AND uid NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE messages ADD COLUMN sync_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attachments ADD COLUMN uid TEXT NULL
    CHECK (uid IS NULL OR (typeof(uid) = 'text' AND length(uid) = 32 AND uid NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE attachments ADD COLUMN sync_seq INTEGER NOT NULL DEFAULT 0;

-- 版(端末ID → その端末での番号のJSONオブジェクト)と「同期のあとに変えた」印。`messages`の版と印は
-- `deleted_at`のもの。既存の行は、版を空にし、印を付けない(docs/sync/index.md「段階」)。
ALTER TABLE tasks ADD COLUMN sync_version TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(sync_version) AND json_type(sync_version) = 'object');
ALTER TABLE tasks ADD COLUMN sync_changed INTEGER NOT NULL DEFAULT 0 CHECK (sync_changed IN (0, 1));
ALTER TABLE task_steps ADD COLUMN sync_version TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(sync_version) AND json_type(sync_version) = 'object');
ALTER TABLE task_steps ADD COLUMN sync_changed INTEGER NOT NULL DEFAULT 0 CHECK (sync_changed IN (0, 1));
ALTER TABLE memories ADD COLUMN sync_version TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(sync_version) AND json_type(sync_version) = 'object');
ALTER TABLE memories ADD COLUMN sync_changed INTEGER NOT NULL DEFAULT 0 CHECK (sync_changed IN (0, 1));
ALTER TABLE messages ADD COLUMN sync_version TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(sync_version) AND json_type(sync_version) = 'object');
ALTER TABLE messages ADD COLUMN sync_changed INTEGER NOT NULL DEFAULT 0 CHECK (sync_changed IN (0, 1));

-- 通常発言が作った時点で続けた、その会話の消えていない最後の通常発言。付けるのはトリガーではなく
-- `db::messages::insert_message`(docs/sync/index.md「構成」)。
ALTER TABLE messages ADD COLUMN prev_uid TEXT NULL
    CHECK (prev_uid IS NULL OR (kind = 'normal' AND typeof(prev_uid) = 'text'
                                AND length(prev_uid) = 32 AND prev_uid NOT GLOB '*[^0-9a-f]*'));

UPDATE tasks SET uid = lower(hex(randomblob(16))), sync_seq = 1;
UPDATE task_steps SET uid = lower(hex(randomblob(16))), sync_seq = 1;
UPDATE memories SET uid = lower(hex(randomblob(16))), sync_seq = 1;
UPDATE messages SET uid = lower(hex(randomblob(16))), sync_seq = 1;
UPDATE attachments SET uid = lower(hex(randomblob(16))), sync_seq = 1;

-- 既存の通常発言の`prev_uid`は、会話ごとに`id`の順で、直前の消えていない通常発言を指す。消えていない
-- 行どうしは1本の列になる。消えていない通常発言を索引を張った一時表に置いてから引く(長く使った
-- 会話でも行数に比例する時間で済む)。
CREATE TEMP TABLE alive_normal AS
SELECT task_id, id, uid FROM messages WHERE kind = 'normal' AND deleted_at IS NULL;

CREATE INDEX temp.alive_normal_chat ON alive_normal (task_id, id);

UPDATE messages
SET prev_uid = (
    SELECT p.uid FROM alive_normal AS p
    WHERE p.task_id IS messages.task_id AND p.id < messages.id
    ORDER BY p.id DESC LIMIT 1
)
WHERE kind = 'normal';

DROP TABLE alive_normal;

CREATE UNIQUE INDEX idx_tasks_uid ON tasks(uid);
CREATE UNIQUE INDEX idx_task_steps_uid ON task_steps(uid);
CREATE UNIQUE INDEX idx_memories_uid ON memories(uid);
CREATE UNIQUE INDEX idx_messages_uid ON messages(uid);
CREATE UNIQUE INDEX idx_attachments_uid ON attachments(uid);

-- 一度付いた`uid`と`prev_uid`は変えない(相手の端末の行と突き合わせる値なので)。
CREATE TRIGGER tasks_uid_fixed
BEFORE UPDATE OF uid ON tasks
WHEN OLD.uid IS NOT NULL AND NEW.uid IS NOT OLD.uid
BEGIN
    SELECT RAISE(ABORT, 'uid does not change once set');
END;

CREATE TRIGGER task_steps_uid_fixed
BEFORE UPDATE OF uid ON task_steps
WHEN OLD.uid IS NOT NULL AND NEW.uid IS NOT OLD.uid
BEGIN
    SELECT RAISE(ABORT, 'uid does not change once set');
END;

CREATE TRIGGER memories_uid_fixed
BEFORE UPDATE OF uid ON memories
WHEN OLD.uid IS NOT NULL AND NEW.uid IS NOT OLD.uid
BEGIN
    SELECT RAISE(ABORT, 'uid does not change once set');
END;

CREATE TRIGGER messages_uid_fixed
BEFORE UPDATE OF uid, prev_uid ON messages
WHEN (OLD.uid IS NOT NULL AND NEW.uid IS NOT OLD.uid) OR NEW.prev_uid IS NOT OLD.prev_uid
BEGIN
    SELECT RAISE(ABORT, 'uid and prev_uid do not change once set');
END;

CREATE TRIGGER attachments_uid_fixed
BEFORE UPDATE OF uid ON attachments
WHEN OLD.uid IS NOT NULL AND NEW.uid IS NOT OLD.uid
BEGIN
    SELECT RAISE(ABORT, 'uid does not change once set');
END;

-- `sync_seq`は、トリガーが振る値(今のカウンター)以外に書き換えない。書き換えるとUPDATEのトリガーが
-- 動かず(下記)、印もカウンターも進まないまま、カウンターより大きい番号が残りうる。
CREATE TRIGGER tasks_seq_from_counter
BEFORE UPDATE OF sync_seq ON tasks
WHEN NEW.sync_seq IS NOT (SELECT counter FROM sync_state WHERE id = 1)
BEGIN
    SELECT RAISE(ABORT, 'sync_seq is assigned only by the triggers');
END;

CREATE TRIGGER task_steps_seq_from_counter
BEFORE UPDATE OF sync_seq ON task_steps
WHEN NEW.sync_seq IS NOT (SELECT counter FROM sync_state WHERE id = 1)
BEGIN
    SELECT RAISE(ABORT, 'sync_seq is assigned only by the triggers');
END;

CREATE TRIGGER memories_seq_from_counter
BEFORE UPDATE OF sync_seq ON memories
WHEN NEW.sync_seq IS NOT (SELECT counter FROM sync_state WHERE id = 1)
BEGIN
    SELECT RAISE(ABORT, 'sync_seq is assigned only by the triggers');
END;

CREATE TRIGGER messages_seq_from_counter
BEFORE UPDATE OF sync_seq ON messages
WHEN NEW.sync_seq IS NOT (SELECT counter FROM sync_state WHERE id = 1)
BEGIN
    SELECT RAISE(ABORT, 'sync_seq is assigned only by the triggers');
END;

CREATE TRIGGER attachments_seq_from_counter
BEFORE UPDATE OF sync_seq ON attachments
WHEN NEW.sync_seq IS NOT (SELECT counter FROM sync_state WHERE id = 1)
BEGIN
    SELECT RAISE(ABORT, 'sync_seq is assigned only by the triggers');
END;

-- INSERTでは、指定の無い`uid`を振り、`sync_seq`を振り、印を付ける。UPDATEでは`sync_seq`を振り直し、
-- 比べる欄(docs/sync/index.md「版と結合」の表)の値が実際に変わったときだけ印を付ける(付いている印は
-- そのまま)。どちらも取り込みにも掛かる。印を外すのは同期の層の書き込みだけ。
--
-- UPDATEのトリガーは`sync_seq`を書かない書き込みにだけ掛け、トリガー自身の書き込みで再び動かない
-- ようにする。
CREATE TRIGGER tasks_sync_insert
AFTER INSERT ON tasks
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE tasks
    SET uid = coalesce(NEW.uid, lower(hex(randomblob(16)))),
        sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER tasks_sync_update
AFTER UPDATE ON tasks
WHEN NEW.sync_seq IS OLD.sync_seq
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE tasks
    SET sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = NEW.sync_changed
            OR NEW.title IS NOT OLD.title
            OR NEW.description IS NOT OLD.description
            OR NEW.deadline IS NOT OLD.deadline
            OR (NEW.archived_at IS NULL) <> (OLD.archived_at IS NULL)
            OR (NEW.deleted_at IS NULL) <> (OLD.deleted_at IS NULL)
    WHERE id = NEW.id;
END;

CREATE TRIGGER task_steps_sync_insert
AFTER INSERT ON task_steps
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE task_steps
    SET uid = coalesce(NEW.uid, lower(hex(randomblob(16)))),
        sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER task_steps_sync_update
AFTER UPDATE ON task_steps
WHEN NEW.sync_seq IS OLD.sync_seq
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE task_steps
    SET sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = NEW.sync_changed
            OR NEW.description IS NOT OLD.description
            OR (NEW.done_at IS NULL) <> (OLD.done_at IS NULL)
            OR (NEW.deleted_at IS NULL) <> (OLD.deleted_at IS NULL)
    WHERE id = NEW.id;
END;

CREATE TRIGGER memories_sync_insert
AFTER INSERT ON memories
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE memories
    SET uid = coalesce(NEW.uid, lower(hex(randomblob(16)))),
        sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER memories_sync_update
AFTER UPDATE ON memories
WHEN NEW.sync_seq IS OLD.sync_seq
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE memories
    SET sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = NEW.sync_changed
            OR NEW.content IS NOT OLD.content
            OR (NEW.deleted_at IS NULL) <> (OLD.deleted_at IS NULL)
    WHERE id = NEW.id;
END;

CREATE TRIGGER messages_sync_insert
AFTER INSERT ON messages
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE messages
    SET uid = coalesce(NEW.uid, lower(hex(randomblob(16)))),
        sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER messages_sync_update
AFTER UPDATE ON messages
WHEN NEW.sync_seq IS OLD.sync_seq
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE messages
    SET sync_seq = (SELECT counter FROM sync_state WHERE id = 1),
        sync_changed = NEW.sync_changed
            OR (NEW.deleted_at IS NULL) <> (OLD.deleted_at IS NULL)
    WHERE id = NEW.id;
END;

CREATE TRIGGER attachments_sync_insert
AFTER INSERT ON attachments
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE attachments
    SET uid = coalesce(NEW.uid, lower(hex(randomblob(16)))),
        sync_seq = (SELECT counter FROM sync_state WHERE id = 1)
    WHERE id = NEW.id;
END;

CREATE TRIGGER attachments_sync_update
AFTER UPDATE ON attachments
WHEN NEW.sync_seq IS OLD.sync_seq
BEGIN
    UPDATE sync_state SET counter = counter + 1 WHERE id = 1;
    UPDATE attachments
    SET sync_seq = (SELECT counter FROM sync_state WHERE id = 1)
    WHERE id = NEW.id;
END;
