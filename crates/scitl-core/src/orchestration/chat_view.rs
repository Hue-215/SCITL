//! 画面に渡す会話の行。どの行をどの順で出すかは`db::messages::list_for_chat`が決め、
//! ここは行ごとの表示の形を足すだけ。

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::attachments::{self, AttachmentKind};
use crate::db::messages::{self, Chat, Kind, Message, Role};
use crate::db::transcripts;
use crate::error::Result;
use crate::orchestration::tool_record::ToolExecutionView;
use crate::orchestration::transcript::SentInput;
use crate::orchestration::turn::require_chat;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct MessageView {
    #[serde(flatten)]
    pub message: Message,
    /// ツール実行記録の行だけが持つ。
    pub tool_execution: Option<ToolExecutionView>,
    /// 添付のうち、中身(テキストの本文・画像)をモデルへ渡していないもののid([`undelivered`])。
    pub undelivered_attachments: Vec<i64>,
}

/// 1つの会話の行を、画面に出す形で返す。タスクが存在しない・削除済みなら`TaskNotFound`
/// (行の無い会話と区別する)。
pub fn list_chat(conn: &Connection, chat: Chat) -> Result<Vec<MessageView>> {
    require_chat(conn, chat)?;
    let rows = messages::list_for_chat(conn, chat)?;
    let mut undelivered = undelivered(conn, chat, &rows)?;
    Ok(rows
        .into_iter()
        .map(|message| MessageView {
            tool_execution: (message.kind == Kind::ToolExecution)
                .then(|| ToolExecutionView::of_content(&message.content)),
            undelivered_attachments: undelivered.remove(&message.id).unwrap_or_default(),
            message,
        })
        .collect())
}

/// ユーザー発言の添付のうち、中身をモデルへ渡していないもののidを、発言のidごとに返す
/// (`docs/spec/architecture/attachments.md`「渡されなかった添付の印」)。渡し方は
/// `attachments::delivery`に従う。
///
/// - テキストは本文を毎ターン送るので、渡している
/// - その他の形式は中身を送らないので、渡していない
/// - 画像は、その発言を入力に含めた試行のうち最後のものの保存(送った形)に載っているかで決める。
///   保存が無いとき、返信のあるターンが答えていれば送った形を保存する前の会話で分からないので
///   含めず、返信が無ければ(失敗・停止したターン)渡していない。応答を生成中の発言もこれに当たる
///   ので、画面はその間の印を出さない
fn undelivered(conn: &Connection, chat: Chat, rows: &[Message]) -> Result<HashMap<i64, Vec<i64>>> {
    let users = rows
        .iter()
        .filter(|m| m.role == Role::User && !m.attachments.is_empty());
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    let has_images = rows
        .iter()
        .flat_map(|m| &m.attachments)
        .any(|a| a.kind == AttachmentKind::Image);
    // 画像が無ければ保存を読まない(会話を読み直すたびに呼ばれるため)。
    let images = if has_images {
        Some(SentImages::load(conn, chat, rows)?)
    } else {
        None
    };
    for message in users {
        let ids: Vec<i64> = message
            .attachments
            .iter()
            .filter(|a| match a.kind {
                AttachmentKind::Text => false,
                AttachmentKind::Other => true,
                AttachmentKind::Image => images
                    .as_ref()
                    .and_then(|images| images.delivered(message.id, a.id))
                    .is_some_and(|delivered| !delivered),
            })
            .map(|a| a.id)
            .collect();
        if !ids.is_empty() {
            out.insert(message.id, ids);
        }
    }
    Ok(out)
}

/// 画像の添付を渡したかを決める材料。
struct SentImages {
    /// 行のidから、その行を入力に含めた最後の保存の、ユーザー発言に載せた画像の実体のハッシュ。
    last_input: HashMap<i64, HashSet<String>>,
    /// 添付のidから実体のハッシュ。
    hashes: HashMap<i64, String>,
    /// 返信のあるターンが答えたユーザー発言。
    replied: HashSet<i64>,
}

impl SentImages {
    fn load(conn: &Connection, chat: Chat, rows: &[Message]) -> Result<Self> {
        let mut last_input = HashMap::new();
        for input in transcripts::inputs_for_chat(conn, chat)? {
            let Some(sent) = SentInput::read(&input) else {
                continue;
            };
            for row in sent.rows {
                last_input.insert(row, sent.images.clone());
            }
        }
        Ok(Self {
            last_input,
            hashes: attachments::file_hashes_in_chat(conn, chat)?,
            replied: replied_users(rows),
        })
    }

    /// 発言`message`の画像`attachment`を渡したか。分からなければ`None`。
    fn delivered(&self, message: i64, attachment: i64) -> Option<bool> {
        match self.last_input.get(&message) {
            Some(images) => Some(
                self.hashes
                    .get(&attachment)
                    .is_some_and(|hash| images.contains(hash)),
            ),
            None if self.replied.contains(&message) => None,
            None => Some(false),
        }
    }
}

/// 返信のあるターンが答えたユーザー発言。発言の直後から次のユーザー発言の手前までに、
/// アシスタントの通常の発言がある。
fn replied_users(rows: &[Message]) -> HashSet<i64> {
    let mut replied = HashSet::new();
    let mut current = None;
    for m in rows.iter().filter(|m| m.kind == Kind::Normal) {
        match m.role {
            Role::User => current = Some(m.id),
            Role::Assistant => replied.extend(current),
            _ => {}
        }
    }
    replied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::attachments::{AttachmentContent, NewAttachment};
    use crate::db::messages::{NewMessage, Origin};
    use crate::orchestration::transcript::{StoredInput, StoredMessage};

    struct Fixture {
        conn: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                conn: db::open_in_memory().unwrap(),
            }
        }

        fn insert(&self, role: Role, origin: Origin, error_kind: Option<&str>) -> i64 {
            messages::insert_message(
                &self.conn,
                NewMessage {
                    chat: Chat::General,
                    role,
                    content: "c",
                    kind: Kind::Normal,
                    origin,
                    error_kind,
                    error_detail: None,
                    partial_reply: None,
                    reasoning: None,
                },
            )
            .unwrap()
        }

        /// ユーザー発言を書き、`attached`の種別と実体のハッシュで添付を付ける。添付のidを返す。
        fn user(&self, attached: &[(AttachmentKind, &str)]) -> (i64, Vec<i64>) {
            let id = self.insert(Role::User, Origin::User, None);
            let ids = attached
                .iter()
                .map(|(kind, hash)| {
                    let content = match kind {
                        AttachmentKind::Text => AttachmentContent::Text("t".to_string()),
                        _ => AttachmentContent::File {
                            hash: hash.to_string(),
                        },
                    };
                    attachments::insert(
                        &self.conn,
                        id,
                        &NewAttachment {
                            original_name: "a".to_string(),
                            mime_type: "image/png".to_string(),
                            kind: *kind,
                            size_bytes: 1,
                            content,
                        },
                    )
                    .unwrap()
                })
                .collect();
            (id, ids)
        }

        fn reply(&self, turn_id: &str) {
            self.insert(
                Role::Assistant,
                Origin::Turn {
                    turn_id,
                    attempt_no: 1,
                },
                None,
            );
        }

        fn fail(&self, turn_id: &str) {
            self.insert(
                Role::Error,
                Origin::Turn {
                    turn_id,
                    attempt_no: 1,
                },
                Some("server_error"),
            );
        }

        /// 試行の保存を書く。`rows`を入力に含め、ユーザー発言1つに`images`を載せる。
        fn save(&self, turn_id: &str, attempt_no: i64, rows: Vec<i64>, images: &[&str]) {
            let input = StoredInput::new(
                rows,
                vec![StoredMessage::User {
                    text: "u".to_string(),
                    images: images.iter().map(|h| h.to_string()).collect(),
                }],
            );
            transcripts::insert(
                &self.conn,
                &transcripts::NewTranscript {
                    chat: Chat::General,
                    turn_id,
                    attempt_no,
                    api_format: "open_ai_compat",
                    model: "m",
                    server: "https://api.example.com",
                    system: "s",
                    settings_system: "s",
                    tools: "[]",
                    prefix_digest: "p",
                    history_start: None,
                    input: &serde_json::to_string(&input).unwrap(),
                    rounds: "[]",
                },
            )
            .unwrap();
        }

        fn undelivered(&self) -> HashMap<i64, Vec<i64>> {
            list_chat(&self.conn, Chat::General)
                .unwrap()
                .into_iter()
                .filter(|v| !v.undelivered_attachments.is_empty())
                .map(|v| (v.message.id, v.undelivered_attachments))
                .collect()
        }
    }

    #[test]
    fn text_is_always_delivered_and_other_files_never_are() {
        let f = Fixture::new();
        let (u, ids) = f.user(&[(AttachmentKind::Text, ""), (AttachmentKind::Other, "z")]);
        f.reply("t1");
        assert_eq!(f.undelivered(), HashMap::from([(u, vec![ids[1]])]));
    }

    #[test]
    fn an_image_is_delivered_when_the_last_attempt_taking_its_message_sent_it() {
        let f = Fixture::new();
        let (u, ids) = f.user(&[(AttachmentKind::Image, "h1"), (AttachmentKind::Image, "h2")]);
        f.reply("t1");
        // 画像を読めないモデルへ送ったあと、読めるモデルで1枚だけ送れた試行で作り直した。
        f.save("t1", 1, vec![u], &[]);
        assert_eq!(f.undelivered(), HashMap::from([(u, ids.clone())]));
        f.save("t1", 2, vec![u], &["h1"]);
        assert_eq!(f.undelivered(), HashMap::from([(u, vec![ids[1]])]));
    }

    #[test]
    fn an_image_of_a_failed_turn_is_not_delivered_even_after_the_next_message() {
        let f = Fixture::new();
        let (failed, failed_ids) = f.user(&[(AttachmentKind::Image, "h1")]);
        f.fail("t1");
        assert_eq!(
            f.undelivered(),
            HashMap::from([(failed, failed_ids.clone())])
        );

        // 次の発言の試行は、失敗した発言も入力に含めるが、画像は直近の発言の分だけを載せる。
        let (next, _) = f.user(&[(AttachmentKind::Image, "h2")]);
        f.reply("t2");
        f.save("t2", 1, vec![failed, next], &["h2"]);
        assert_eq!(f.undelivered(), HashMap::from([(failed, failed_ids)]));
    }

    #[test]
    fn an_image_answered_before_attempts_were_saved_is_left_unknown() {
        let f = Fixture::new();
        f.user(&[(AttachmentKind::Image, "h1")]);
        f.reply("t1");
        assert_eq!(f.undelivered(), HashMap::new());
    }
}
