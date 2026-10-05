-- モデルに送った形(docs/spec/data-model/messages.md turn_transcripts)。返信のある試行ごとに1行を
-- 返信の行と同じトランザクションで書き、書き換えない。会話ログはmessagesが正で、ここはモデルへ
-- 並べるときだけ読む。

-- 送ったシステムプロンプトとツール定義の本文。試行ごとにほぼ同じなので1度だけ置く。
CREATE TABLE transcript_blobs (
    digest  TEXT PRIMARY KEY,
    body    TEXT NOT NULL
);

CREATE TABLE turn_transcripts (
    id                      INTEGER PRIMARY KEY,
    task_id                 INTEGER NULL REFERENCES tasks(id),
    turn_id                 TEXT NOT NULL,
    attempt_no              INTEGER NOT NULL,
    api_format              TEXT NOT NULL,
    model                   TEXT NOT NULL,
    system_digest           TEXT NOT NULL REFERENCES transcript_blobs(digest),
    settings_system_digest  TEXT NOT NULL REFERENCES transcript_blobs(digest),
    tools_digest            TEXT NOT NULL REFERENCES transcript_blobs(digest),
    prefix_digest           TEXT NOT NULL,
    history_start           INTEGER NULL,
    input                   TEXT NOT NULL CHECK (json_valid(input)),
    rounds                  TEXT NOT NULL CHECK (json_valid(rounds)),
    created_at              TEXT NOT NULL,
    UNIQUE (turn_id, attempt_no)
);

CREATE INDEX idx_turn_transcripts_task ON turn_transcripts(task_id);
