//! 画面に渡す会話の行。どの行をどの順で出すかは`db::messages::list_for_chat`が決め、
//! ここは行ごとの表示の形を足すだけ。

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use crate::attachments::{delivery_without_model, revealable, Delivery, CAN_REVEAL};
use crate::db::attachments::{self, AttachmentKind};
use crate::db::messages::{self, Chat, Kind, Message, ReplyRecords, ResolvedPart, Role};
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
    /// ツール実行記録の行(会話に独立して並ぶもの。操作の記録)だけが持つ。
    pub tool_execution: Option<ToolExecutionView>,
    /// ターンの返信の行(アシスタント発言・エラー発言)だけが持つ、そのターンの中身。起きた順。
    pub parts: Vec<PartView>,
    /// 添付のうち、中身(テキストの本文・画像)をモデルへ渡していないもののid([`undelivered`])。
    pub undelivered_attachments: Vec<i64>,
    /// 添付のうち、押して入っているフォルダを開けるもののid(`attachments::revealable`)。
    /// 開けないOS(Android)では空。
    pub revealable_attachments: Vec<i64>,
}

/// ターンの中身の1要素の表示。`round`は1始まりのラウンドの番号。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PartView {
    Reasoning {
        round: u32,
        text: String,
    },
    Text {
        round: u32,
        text: String,
    },
    /// `id`は実行記録の行のid。
    Tool {
        round: u32,
        id: i64,
        execution: ToolExecutionView,
    },
}

impl PartView {
    fn of(part: ResolvedPart) -> Self {
        match part {
            ResolvedPart::Reasoning { round, text } => Self::Reasoning {
                round,
                text: text.to_string(),
            },
            ResolvedPart::Text { round, text } => Self::Text {
                round,
                text: text.to_string(),
            },
            ResolvedPart::Tool { round, record } => Self::Tool {
                round,
                id: record.id,
                execution: ToolExecutionView::of_content(&record.content),
            },
        }
    }
}

/// 1つの会話の行を、画面に出す形で返す。タスクが存在しない・削除済みなら`TaskNotFound`
/// (行の無い会話と区別する)。返信の中身が指す実行記録は、独立した行として返さず、返信の
/// `parts`の中に起きた順で並べる。
pub fn list_chat(conn: &Connection, chat: Chat) -> Result<Vec<MessageView>> {
    require_chat(conn, chat)?;
    let rows = messages::list_for_chat(conn, chat)?;
    let mut undelivered = undelivered(conn, chat, &rows)?;
    let records = ReplyRecords::of(&rows);
    let parts: Vec<Vec<PartView>> = rows
        .iter()
        .map(|m| records.resolve(m).into_iter().map(PartView::of).collect())
        .collect();
    let referenced: Vec<bool> = rows.iter().map(|m| records.contains(m.id)).collect();
    Ok(rows
        .into_iter()
        .zip(parts)
        .zip(referenced)
        .filter(|(_, referenced)| !referenced)
        .map(|((message, parts), _)| MessageView {
            tool_execution: (message.kind == Kind::ToolExecution)
                .then(|| ToolExecutionView::of_content(&message.content)),
            parts,
            undelivered_attachments: undelivered.remove(&message.id).unwrap_or_default(),
            revealable_attachments: revealable_ids(&message, CAN_REVEAL),
            message,
        })
        .collect())
}

fn revealable_ids(message: &Message, can_reveal: bool) -> Vec<i64> {
    message
        .attachments
        .iter()
        .filter(|a| revealable(a.kind, can_reveal))
        .map(|a| a.id)
        .collect()
}

/// ユーザー発言の添付のうち、中身をモデルへ渡していないもののidを、発言のidごとに返す
/// (`docs/spec/architecture/attachments.md`「渡されなかった添付の印」)。示すのは、その発言に
/// 答えた試行で渡したかで、今もモデルが見ているかではない。
///
/// - テキスト・その他は、モデルによらず渡し方が決まる(`attachments::delivery_without_model`)
/// - 画像は、表示される返信のある試行のうち、その発言を入力に含めたものの送った形の保存に載って
///   いるかで決める。そうした保存が無いとき、その発言に答えた返信が無く、あとの発言に答えた返信が
///   あれば渡していない。それ以外は分からないので含めない(答えた返信があれば送った形を保存する前の
///   会話。あとに返信が1つも無ければ、失敗・停止したターンか応答を生成中の発言で、再試行すれば渡る)
fn undelivered(conn: &Connection, chat: Chat, rows: &[Message]) -> Result<HashMap<i64, Vec<i64>>> {
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
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    for message in rows.iter().filter(|m| m.role == Role::User) {
        let ids: Vec<i64> = message
            .attachments
            .iter()
            .filter(|a| match delivery_without_model(a.kind) {
                Some(delivery) => delivery == Delivery::NameOnly,
                None => images
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
    /// 行のidから、その行を入力に含めた保存の、ユーザー発言に載せた画像の実体のハッシュ。
    /// 表示される返信のある試行の保存だけを使うので、1つの行を含む保存は1つまで。
    inputs: HashMap<i64, HashSet<String>>,
    /// 添付のidから実体のハッシュ。
    hashes: HashMap<i64, String>,
    /// 返信の有無から見たユーザー発言。
    replies: Replies,
}

impl SentImages {
    fn load(conn: &Connection, chat: Chat, rows: &[Message]) -> Result<Self> {
        // 捨てた試行・削除したターンの保存は、以後モデルへ並べないので見ない
        // (`orchestration::history`が並べる保存と同じ範囲)。
        let replied_attempts: HashSet<(&str, i64)> = rows
            .iter()
            .filter(|m| m.kind == Kind::Normal && m.role == Role::Assistant)
            .filter_map(|m| Some((m.turn_id.as_deref()?, m.attempt_no?)))
            .collect();
        let mut inputs = HashMap::new();
        for (turn_id, attempt_no, input) in transcripts::inputs_for_chat(conn, chat)? {
            if !replied_attempts.contains(&(turn_id.as_str(), attempt_no)) {
                continue;
            }
            let Some(sent) = SentInput::read(&input) else {
                continue;
            };
            for row in sent.rows {
                inputs.insert(row, sent.images.clone());
            }
        }
        Ok(Self {
            inputs,
            hashes: attachments::file_hashes_in_chat(conn, chat)?,
            replies: Replies::of(rows),
        })
    }

    /// 発言`message`の画像`attachment`を渡したか。分からなければ`None`。
    fn delivered(&self, message: i64, attachment: i64) -> Option<bool> {
        match self.inputs.get(&message) {
            Some(images) => Some(
                self.hashes
                    .get(&attachment)
                    .is_some_and(|hash| images.contains(hash)),
            ),
            None if self.replies.followed.contains(&message)
                && !self.replies.answered.contains(&message) =>
            {
                Some(false)
            }
            None => None,
        }
    }
}

/// 返信の有無から見たユーザー発言。返信は、アシスタントの通常の発言。
struct Replies {
    /// 返信のあるターンが答えた発言。発言の直後から次のユーザー発言の手前までに返信がある。
    answered: HashSet<i64>,
    /// あとに返信がある発言(自身に答えた返信を含む)。
    followed: HashSet<i64>,
}

impl Replies {
    fn of(rows: &[Message]) -> Self {
        let mut answered = HashSet::new();
        let mut users = Vec::new();
        let mut followed_until = 0;
        for m in rows.iter().filter(|m| m.kind == Kind::Normal) {
            match m.role {
                Role::User => users.push(m.id),
                Role::Assistant => {
                    answered.extend(users.last().copied());
                    followed_until = users.len();
                }
                _ => {}
            }
        }
        // 最後の返信より前の発言だけが、あとに返信を持つ。
        users.truncate(followed_until);
        Self {
            answered,
            followed: users.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::attachments::{AttachmentContent, NewAttachment};
    use crate::db::messages::{NewMessage, OperationSource, Origin, ReplyPart};
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
                    parts: matches!(origin, Origin::Turn { .. }).then_some(&[]),
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

        fn reply(&self, turn_id: &str, attempt_no: i64) -> i64 {
            self.insert(
                Role::Assistant,
                Origin::Turn {
                    turn_id,
                    attempt_no,
                },
                None,
            )
        }

        fn fail(&self, turn_id: &str, attempt_no: i64) {
            self.insert(
                Role::Error,
                Origin::Turn {
                    turn_id,
                    attempt_no,
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

    /// 返信の中身が指す実行記録は、独立した行として返さず、返信の中身の位置に起きた順で並ぶ。
    /// 操作の記録は会話に並ぶ行のまま。
    #[test]
    fn records_of_a_reply_are_listed_inside_it_in_order() {
        let f = Fixture::new();
        let write = |role, content: &str, kind, origin, parts: Option<&[ReplyPart]>| {
            messages::insert_message(
                &f.conn,
                NewMessage {
                    chat: Chat::General,
                    role,
                    content,
                    kind,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts,
                },
            )
            .unwrap()
        };
        let turn = Origin::Turn {
            turn_id: "t",
            attempt_no: 1,
        };
        let record = r#"{"tool":"list_tasks","arguments":{},"result":{}}"#;
        write(Role::User, "質問", Kind::Normal, Origin::User, None);
        let called = write(Role::Tool, record, Kind::ToolExecution, turn, None);
        let operation = write(
            Role::Tool,
            record,
            Kind::ToolExecution,
            Origin::Operation(OperationSource::Ui),
            None,
        );
        let text = |round, text: &str| ReplyPart::Text {
            round,
            text: text.to_string(),
        };
        write(
            Role::Assistant,
            "",
            Kind::Normal,
            turn,
            Some(&[
                text(1, "調べます"),
                ReplyPart::Tool {
                    round: 1,
                    record: called,
                },
                text(2, "本題"),
            ]),
        );

        let views = list_chat(&f.conn, Chat::General).unwrap();

        let ids: Vec<i64> = views.iter().map(|v| v.message.id).collect();
        assert!(!ids.contains(&called));
        assert!(ids.contains(&operation));
        let reply = views.last().unwrap();
        let parts: Vec<String> = reply
            .parts
            .iter()
            .map(|p| match p {
                PartView::Text { text, .. } => text.clone(),
                PartView::Tool { id, execution, .. } => {
                    assert_eq!(*id, called);
                    serde_json::to_value(execution).unwrap()["tool"]
                        .as_str()
                        .unwrap()
                        .to_string()
                }
                PartView::Reasoning { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(parts, ["調べます", "list_tasks", "本題"]);
    }

    #[test]
    fn text_is_always_delivered_and_other_files_never_are() {
        let f = Fixture::new();
        let (u, ids) = f.user(&[(AttachmentKind::Text, ""), (AttachmentKind::Other, "z")]);
        f.reply("t1", 1);
        assert_eq!(f.undelivered(), HashMap::from([(u, vec![ids[1]])]));
    }

    /// 開けるのはその他の添付だけで、開けないOSでは何も開けない。
    #[test]
    fn only_other_files_are_revealable_and_none_where_folders_cannot_be_opened() {
        let f = Fixture::new();
        let (u, ids) = f.user(&[
            (AttachmentKind::Text, ""),
            (AttachmentKind::Image, "h1"),
            (AttachmentKind::Other, "z"),
        ]);
        let message = messages::list_for_chat(&f.conn, Chat::General)
            .unwrap()
            .into_iter()
            .find(|m| m.id == u)
            .unwrap();
        assert_eq!(revealable_ids(&message, true), [ids[2]]);
        assert_eq!(revealable_ids(&message, false), Vec::<i64>::new());
    }

    #[test]
    fn an_image_is_delivered_when_the_shown_attempt_sent_it() {
        let f = Fixture::new();
        let (u, ids) = f.user(&[(AttachmentKind::Image, "h1"), (AttachmentKind::Image, "h2")]);
        // 画像を読めないモデルへ送ったあと、読めるモデルで1枚だけ送れた試行で作り直した。
        f.reply("t1", 1);
        f.save("t1", 1, vec![u], &[]);
        assert_eq!(f.undelivered(), HashMap::from([(u, ids.clone())]));
        f.reply("t1", 2);
        f.save("t1", 2, vec![u], &["h1"]);
        assert_eq!(f.undelivered(), HashMap::from([(u, vec![ids[1]])]));
    }

    /// 失敗したターンの画像は、あとの返信が載せずに書かれたら渡していない。まだ返信が1つも無い
    /// うちは、再試行すれば渡るので分からない扱いにする。
    #[test]
    fn an_image_of_a_failed_turn_is_not_delivered_once_a_later_reply_left_it_out() {
        let f = Fixture::new();
        let (failed, failed_ids) = f.user(&[(AttachmentKind::Image, "h1")]);
        f.fail("t1", 1);
        assert_eq!(f.undelivered(), HashMap::new());

        // 次の発言の試行は、失敗した発言も入力に含めるが、画像は直近の発言の分だけを載せる。
        let (next, _) = f.user(&[(AttachmentKind::Image, "h2")]);
        assert_eq!(f.undelivered(), HashMap::new());
        f.reply("t2", 1);
        f.save("t2", 1, vec![failed, next], &["h2"]);
        assert_eq!(
            f.undelivered(),
            HashMap::from([(failed, failed_ids.clone())])
        );

        // あとの返信の送った形にその行が含まれない(間引かれた等)ときも、渡していない。
        let g = Fixture::new();
        let (failed, failed_ids) = g.user(&[(AttachmentKind::Image, "h1")]);
        g.fail("t1", 1);
        let (next, _) = g.user(&[]);
        g.reply("t2", 1);
        g.save("t2", 1, vec![next], &[]);
        assert_eq!(g.undelivered(), HashMap::from([(failed, failed_ids)]));

        // 失敗が続いたあとの返信は、それより前の失敗した発言すべてについて決める。最後の返信より
        // 後ろの失敗は、まだ分からない。
        let h = Fixture::new();
        let (first, first_ids) = h.user(&[(AttachmentKind::Image, "h1")]);
        h.fail("t1", 1);
        let (second, second_ids) = h.user(&[(AttachmentKind::Image, "h2")]);
        h.fail("t2", 1);
        h.user(&[]);
        h.reply("t3", 1);
        h.user(&[(AttachmentKind::Image, "h3")]);
        h.fail("t4", 1);
        assert_eq!(
            h.undelivered(),
            HashMap::from([(first, first_ids), (second, second_ids)])
        );
    }

    /// 送れた試行を作り直して失敗した・返信を消したら、その保存はもう並べない。あとに返信が
    /// 無ければ、失敗したターンと同じく分からない扱いになる。
    #[test]
    fn a_saved_attempt_that_is_no_longer_shown_does_not_count() {
        let f = Fixture::new();
        let (u, _) = f.user(&[(AttachmentKind::Image, "h1")]);
        f.reply("t1", 1);
        f.save("t1", 1, vec![u], &["h1"]);
        assert_eq!(f.undelivered(), HashMap::new());
        f.fail("t1", 2);
        assert_eq!(f.undelivered(), HashMap::new());

        let g = Fixture::new();
        let (u, ids) = g.user(&[(AttachmentKind::Image, "h1")]);
        let reply = g.reply("t1", 1);
        g.save("t1", 1, vec![u], &["h1"]);
        messages::soft_delete_message(&g.conn, reply).unwrap();
        assert_eq!(g.undelivered(), HashMap::new());
        let (next, _) = g.user(&[]);
        g.reply("t2", 1);
        g.save("t2", 1, vec![u, next], &[]);
        assert_eq!(g.undelivered(), HashMap::from([(u, ids)]));
    }

    #[test]
    fn an_image_answered_before_attempts_were_saved_is_left_unknown() {
        let f = Fixture::new();
        f.user(&[(AttachmentKind::Image, "h1")]);
        f.reply("t1", 1);
        assert_eq!(f.undelivered(), HashMap::new());
    }
}
