-- 資格情報ストアの端末IDの項目と同じ値(docs/sync/index.md 10節)。同期のたびに両方を替え、DBの値が
-- 項目の値と違えば、写したDBか戻したDBとみなして端末IDを作り直す。
--
-- 項目を作るまでは、どの項目の値とも合わない乱数を入れておく。前からある項目は値をDBに持っていないので、
-- 次の同期で端末IDを1度作り直す(要らない作り直しは害が無い)。`NOT NULL`の列は既定値を要するので、
-- 形だけ合う値で足してから乱数で埋める。
ALTER TABLE sync_state ADD COLUMN device_token TEXT NOT NULL
    DEFAULT '00000000000000000000000000000000'
    CHECK (typeof(device_token) = 'text' AND length(device_token) = 32
           AND device_token NOT GLOB '*[^0-9a-f]*');

UPDATE sync_state SET device_token = lower(hex(randomblob(16)));
