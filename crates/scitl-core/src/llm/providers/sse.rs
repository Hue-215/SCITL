//! SSE(`text/event-stream`)の応答をイベントの`data`に分ける。方言によらない部分だけを持ち、
//! `data`の中身(JSON)の読み方は各アダプタが持つ。
//!
//! 読み方はWHATWGのHTML仕様「Server-sent events」の解釈に従う。使わない欄(`event`・`id`・
//! `retry`)とコメントは読み飛ばす。

use crate::llm::{ErrorDetail, LlmError, SentSecrets};

/// 応答が`text/event-stream`か。ストリーミングを頼んでも、無視して1つのJSONで返すサーバーがある。
pub(super) fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// 応答の本文を読み、イベントの`data`を届いた順に`on_data`へ渡す。`on_data`が`Ok(true)`を
/// 返したら(方言の終わりの合図)、残りを読まずに終える。
///
/// 返り値は、終わりの合図で止めたか。`false`なら、合図の無いまま本文が終わった。
///
/// 読む量の合計に上限([`MAX_STREAM_BYTES`])を掛ける。ストリーミングでは待つ時間の上限が
/// 無通信の間隔だけなので(`net::RequestTimeout::BetweenReads`)、送り続けるサーバーを
/// 時間では止められないため。
pub(super) async fn read_data(
    mut response: reqwest::Response,
    secrets: &SentSecrets,
    mut on_data: impl FnMut(String) -> Result<bool, LlmError>,
) -> Result<bool, LlmError> {
    let mut decoder = SseDecoder::default();
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| LlmError::from_body_read(e, secrets))?
    {
        total = total.saturating_add(chunk.len());
        if total > MAX_STREAM_BYTES {
            return Err(LlmError::InvalidResponse(ErrorDetail::internal(
                "the event stream is too large",
            )));
        }
        for data in decoder.push(&chunk)? {
            if on_data(data)? {
                return Ok(true);
            }
        }
    }
    match decoder.finish()? {
        Some(data) => on_data(data),
        None => Ok(false),
    }
}

/// 1つのイベント(行の途中を含む)として貯める大きさの上限。区切りを送らないサーバーに、
/// 手元のメモリを使い切らせないため。正常な応答の1イベントはこれよりずっと小さい。
const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// 1つの応答として読む量の上限([`read_data`])。1回のモデル呼び出しの応答はこれよりずっと小さい。
const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// 届いた順にバイト列を受け取り、区切りまで届いたイベントの`data`を返す。チャンクの境界は
/// 行やUTF-8の文字の途中にあってもよい。
#[derive(Default)]
pub(super) struct SseDecoder {
    /// まだ行の終わりが届いていないバイト列。
    pending: Vec<u8>,
    /// 組み立て中のイベントの`data`。`data`の行が1つも無ければ`None`。
    data: Option<String>,
    /// 直前の行が`\r`で終わった(続く`\n`は同じ改行の一部なので読み飛ばす)。
    after_cr: bool,
    /// 先頭のBOMを見終えた。
    started: bool,
    /// `pending`のうち、改行が無いと調べ終えた長さ。改行の届かない長い行を、チャンクが届く
    /// たびに先頭から調べ直さないため。
    scanned: usize,
}

impl SseDecoder {
    /// `bytes`を足し、区切りまで届いたイベントの`data`を届いた順に返す。
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, LlmError> {
        // 空のチャンクで`after_cr`を消さない。
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let mut bytes = bytes;
        if self.after_cr {
            self.after_cr = false;
            if let Some(rest) = bytes.strip_prefix(b"\n") {
                bytes = rest;
            }
        }
        self.pending.extend_from_slice(bytes);
        if !self.started {
            if self.pending.len() < UTF8_BOM.len() && UTF8_BOM.starts_with(&self.pending) {
                return Ok(Vec::new());
            }
            self.started = true;
            if self.pending.starts_with(UTF8_BOM) {
                self.pending.drain(..UTF8_BOM.len());
            }
        }

        let mut events = Vec::new();
        let mut start = 0;
        let mut from = self.scanned;
        while let Some(offset) = self.pending[from..]
            .iter()
            .position(|b| matches!(b, b'\n' | b'\r'))
        {
            let end = from + offset;
            let mut next = end + 1;
            if self.pending[end] == b'\r' {
                match self.pending.get(next) {
                    Some(b'\n') => next += 1,
                    // `\r\n`の`\n`が次のチャンクで届くかもしれない。
                    None => self.after_cr = true,
                    Some(_) => {}
                }
            }
            if let Some(data) = self.line(start, end)? {
                events.push(data);
            }
            start = next;
            from = next;
        }
        self.pending.drain(..start);
        self.scanned = self.pending.len();
        self.check_size()?;
        Ok(events)
    }

    /// 応答の終わり。区切りの空行が無いまま終わったイベントも返す(仕様では捨てるが、最後の
    /// 空行を省くサーバーがあり、捨てると最後の`data`を黙って失う)。
    pub(super) fn finish(&mut self) -> Result<Option<String>, LlmError> {
        // 改行の無い最後の行。空ではないので、イベントを区切らずに`data`へ足すだけ。
        if !self.pending.is_empty() {
            self.line(0, self.pending.len())?;
            self.pending.clear();
            self.scanned = 0;
        }
        Ok(self.data.take())
    }

    /// `pending[start..end]`の1行を読む。空行ならイベントを区切り、その`data`を返す。
    fn line(&mut self, start: usize, end: usize) -> Result<Option<String>, LlmError> {
        let line = std::str::from_utf8(&self.pending[start..end]).map_err(|_| {
            LlmError::InvalidResponse(ErrorDetail::internal("the event stream is not valid UTF-8"))
        })?;
        if line.is_empty() {
            return Ok(self.data.take());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            }
        }
        Ok(None)
    }

    fn check_size(&self) -> Result<(), LlmError> {
        let held = self.pending.len() + self.data.as_ref().map_or(0, String::len);
        if held > MAX_EVENT_BYTES {
            return Err(LlmError::InvalidResponse(ErrorDetail::internal(
                "an event in the event stream is too large",
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_chunks(chunks: &[&[u8]]) -> Vec<String> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        events
    }

    #[test]
    fn splits_events_on_blank_lines() {
        assert_eq!(
            decode_chunks(&[b"data: a\n\ndata: b\n\n"]),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn joins_multiple_data_lines_with_a_line_feed() {
        assert_eq!(decode_chunks(&[b"data: a\ndata: b\n\n"]), vec!["a\nb"]);
    }

    #[test]
    fn accepts_every_line_ending_even_split_across_chunks() {
        assert_eq!(
            decode_chunks(&[b"data: a\r", b"\n\r", b"\ndata: b\r\rdata: c\n\n"]),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn keeps_a_character_split_across_chunks() {
        let bytes = "data: 日本\n\n".as_bytes();
        // 「日」の途中で切る。
        let (head, tail) = bytes.split_at(7);
        assert_eq!(decode_chunks(&[head, tail]), vec!["日本"]);
    }

    #[test]
    fn ignores_comments_and_other_fields() {
        assert_eq!(
            decode_chunks(&[b": keep-alive\n\nevent: message\nid: 1\nretry: 10\ndata: a\n\n"]),
            vec!["a"]
        );
    }

    #[test]
    fn a_blank_line_without_data_is_not_an_event() {
        assert_eq!(
            decode_chunks(&[b"\n\nevent: ping\n\n"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn strips_only_one_leading_space_and_takes_a_bare_field_as_empty() {
        assert_eq!(
            decode_chunks(&[b"data:a\n\ndata:  b\n\ndata\n\n"]),
            vec!["a", " b", ""]
        );
    }

    #[test]
    fn strips_a_leading_bom_even_when_split() {
        assert_eq!(decode_chunks(&[b"\xEF\xBB", b"\xBFdata: a\n\n"]), vec!["a"]);
    }

    #[test]
    fn returns_an_event_left_open_at_the_end() {
        assert_eq!(decode_chunks(&[b"data: a\n\ndata: b"]), vec!["a", "b"]);
        assert_eq!(decode_chunks(&[b"data: a\ndata: b\n"]), vec!["a\nb"]);
    }

    #[test]
    fn an_empty_chunk_keeps_a_pending_carriage_return() {
        assert_eq!(
            decode_chunks(&[b"data: a\r", b"", b"\ndata: b\r\n\r\n"]),
            vec!["a\nb"]
        );
    }

    #[test]
    fn finds_the_end_of_a_line_that_arrives_in_many_chunks() {
        let mut decoder = SseDecoder::default();
        for piece in [&b"data: "[..], b"a", b"b", b"c"] {
            assert!(decoder.push(piece).unwrap().is_empty());
        }
        assert_eq!(decoder.push(b"\n\n").unwrap(), vec!["abc"]);
    }

    #[test]
    fn rejects_invalid_utf8() {
        let mut decoder = SseDecoder::default();
        assert!(matches!(
            decoder.push(b"data: \xFF\n\n"),
            Err(LlmError::InvalidResponse(_))
        ));
    }

    #[test]
    fn rejects_an_event_that_grows_past_the_limit() {
        let mut decoder = SseDecoder::default();
        let line = vec![b'a'; MAX_EVENT_BYTES / 4];
        decoder.push(b"data: ").unwrap();
        let result = (0..5).try_for_each(|_| decoder.push(&line).map(drop));
        assert!(matches!(result, Err(LlmError::InvalidResponse(_))));
    }
}
