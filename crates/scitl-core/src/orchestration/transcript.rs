//! モデルに送った形の保存の形(`docs/spec/rebuild/architecture.md`「送った形のまま積む」)。
//! 行の読み書きは`db::transcripts`。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::transcripts::digest;
use crate::llm::{ChatMessage, InlineImage, Replay, ToolArguments, ToolCallRequest, ToolSchema};

/// 保存する発言1つ。`llm::ChatMessage`の段階の形だが、`llm`の型を変えてもそのまま保存の形が
/// 変わらないよう、別の型で持つ。画像は実体の代わりに、添付の実体のハッシュを持つ。
///
/// 外部タグの形(`{"assistant": {...}}`)で直列化する。内部タグの形(`"role"`のキー)は、読むときに
/// 値をいったん溜めてから読み直すため、思考の生ブロック([`Replay`])を読めない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StoredMessage {
    User {
        text: String,
        images: Vec<String>,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<StoredToolCall>,
        replay: Replay,
    },
    Tool {
        tool_call_id: Option<String>,
        content: String,
        images: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct StoredToolCall {
    id: Option<String>,
    name: String,
    arguments: ToolArguments,
}

impl StoredMessage {
    /// 発言を保存する形にする。システムプロンプト(別に保存する)と、添付から読み出したもので
    /// ない画像を伴う発言は保存できないので`None`。
    pub(super) fn of(message: &ChatMessage) -> Option<Self> {
        Some(match message {
            ChatMessage::System(_) => return None,
            ChatMessage::User { text, images } => Self::User {
                text: text.as_str().to_string(),
                images: sources(images)?,
            },
            ChatMessage::Assistant {
                content,
                tool_calls,
                replay,
            } => Self::Assistant {
                content: content.clone(),
                tool_calls: tool_calls.iter().map(StoredToolCall::of).collect(),
                replay: replay.clone(),
            },
            ChatMessage::Tool {
                tool_call_id,
                content,
                images,
            } => Self::Tool {
                tool_call_id: tool_call_id.clone(),
                content: content.as_str().to_string(),
                images: sources(images)?,
            },
        })
    }

    /// 発言の列を保存する形にする。1つでも保存できなければ`None`。
    pub(super) fn all_of(messages: &[ChatMessage]) -> Option<Vec<Self>> {
        messages.iter().map(Self::of).collect()
    }
}

impl StoredToolCall {
    fn of(call: &ToolCallRequest) -> Self {
        Self {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        }
    }
}

fn sources(images: &[InlineImage]) -> Option<Vec<String>> {
    images
        .iter()
        .map(|image| image.source().map(str::to_string))
        .collect()
}

/// 試行の入力。入力に含めた行(ユーザー発言と、それに置いた操作の記録)のidを添える。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct StoredInput {
    pub(super) rows: Vec<i64>,
    pub(super) messages: Vec<StoredMessage>,
}

/// 1ターンで送った形のうち、試行によらない部分(`db::transcripts::NewTranscript`に渡す)。
pub(super) struct SavedTurn {
    pub(super) system: String,
    /// そのとき設定から作ったシステムプロンプト。先頭と違えば入力に変更の通知を置いた。
    pub(super) settings_system: String,
    pub(super) tools: String,
    pub(super) prefix_digest: String,
    pub(super) history_start: Option<i64>,
    /// [`StoredInput`]を直列化したもの。
    pub(super) input: String,
    /// [`StoredMessage`]の列を直列化したもの。
    pub(super) rounds: String,
}

/// 渡したツール定義の一覧の本文。定義が変わったかを本文の指紋で見分けるため、毎回同じ形に
/// 直列化する。
pub(super) fn tools_body(tools: &[ToolSchema]) -> String {
    let tools: Vec<_> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name(),
                "description": t.description(),
                "parameters": t.parameters(),
            })
        })
        .collect();
    serde_json::to_string(&tools).expect("tool definitions serialize")
}

/// 並べた発言列の指紋の連鎖(`docs/spec/rebuild/architecture.md`「思考を送り返す範囲」)。
/// システムプロンプトとツール定義から始め、発言を1つ並べるたびに、その発言の形と前の指紋を
/// 合わせて取り直す。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PrefixDigest(String);

impl PrefixDigest {
    pub(super) fn start(system: &str, tools: &str) -> Self {
        Self(digest(&format!("{}\n{}", digest(system), digest(tools))))
    }

    pub(super) fn push(&mut self, message: &StoredMessage) {
        let message = serde_json::to_string(message).expect("a stored message serializes");
        self.0 = digest(&format!("{}\n{}", self.0, message));
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::PromptText;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(PromptText::user_message(text, None))
    }

    #[test]
    fn stores_images_by_the_hash_of_the_attachment_they_were_read_from() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let read = ChatMessage::User {
            text: PromptText::user_message("u", None),
            images: vec![image.clone().with_source("abc")],
        };
        assert_eq!(
            StoredMessage::of(&read),
            Some(StoredMessage::User {
                text: PromptText::user_message("u", None).as_str().to_string(),
                images: vec!["abc".to_string()],
            })
        );

        // 添付から読み出したものでない画像は、実体を持たないので保存できない。
        let unknown = ChatMessage::User {
            text: PromptText::user_message("u", None),
            images: vec![image],
        };
        assert_eq!(StoredMessage::of(&unknown), None);
        assert_eq!(StoredMessage::all_of(&[user("a"), unknown]), None);
    }

    #[test]
    fn a_stored_message_reads_back_to_the_same_value() {
        let call = |id: &str, raw: &str| ToolCallRequest {
            id: Some(id.to_string()),
            name: "search".to_string(),
            arguments: ToolArguments::parse(raw.to_string()),
        };
        // 読めなかった引数も、モデルが出した生の文字列のまま往復する。
        let message = StoredMessage::of(&ChatMessage::Assistant {
            content: Some("a".to_string()),
            tool_calls: vec![call("call_1", "{\"q\":1}"), call("call_2", "{\"q\":")],
            replay: Replay::default(),
        })
        .unwrap();
        let text = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<StoredMessage>(&text).unwrap(),
            message
        );
    }

    /// 思考の生ブロックを持つ発言も、受け取ったままの形で読み戻せる。
    #[test]
    fn a_message_with_a_replay_reads_back_verbatim() {
        let replay: Replay =
            serde_json::from_str(r#"[{"type":"thinking","thinking":"","signature":"sig"}]"#)
                .unwrap();
        let message = StoredMessage::Assistant {
            content: None,
            tool_calls: Vec::new(),
            replay,
        };
        let text = serde_json::to_string(&message).unwrap();
        assert!(text.contains(r#"{"type":"thinking","thinking":"","signature":"sig"}"#));
        assert_eq!(
            serde_json::from_str::<StoredMessage>(&text).unwrap(),
            message
        );
    }

    /// 同じものを同じ順に並べれば同じ指紋になり、何か1つ変われば変わる。
    #[test]
    fn the_prefix_digest_follows_everything_placed_so_far() {
        let digest_of = |system: &str, texts: &[&str]| {
            let mut digest = PrefixDigest::start(system, "[]");
            for text in texts {
                digest.push(&StoredMessage::of(&user(text)).unwrap());
            }
            digest
        };
        assert_eq!(digest_of("s", &["a", "b"]), digest_of("s", &["a", "b"]));
        assert_ne!(digest_of("s", &["a", "b"]), digest_of("s", &["a", "c"]));
        assert_ne!(digest_of("s", &["a", "b"]), digest_of("s", &["b", "a"]));
        assert_ne!(digest_of("s", &["a"]), digest_of("t", &["a"]));
    }
}
