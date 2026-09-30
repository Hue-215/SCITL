-- 送った形の保存に送り先を足す(docs/spec/rebuild/data-model.md turn_transcripts)。思考は
-- 同じ送り先にだけ送り返す(docs/spec/principles.md 3節「思考は受け取ったまま送り返す」)。
-- 足す前の行はNULLで、送り先が分からないので使わない。
ALTER TABLE turn_transcripts ADD COLUMN server TEXT NULL;
