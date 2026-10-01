use std::sync::LazyLock;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::attachments::{self, Delivery};
use crate::db::attachments::{self as db_attachments, AttachmentContent, AttachmentKind};
use crate::db::messages::Chat;
use crate::error::{CoreError, Result};
use crate::llm::{AttachmentNote, ToolSchema};

use super::args::Args;
use super::{InternalTool, Run, ToolOutput};

pub const NAME: &str = "read_attachment";

pub(super) const TOOL: InternalTool = InternalTool {
    schema,
    run: Run::ReadAttachment,
};

/// 総合チャット・タスクチャットで同じ形。対象の会話は文脈から固定するので引数に取らず、添付
/// IDだけを選ばせる。
pub fn schema() -> &'static ToolSchema {
    static SCHEMA: LazyLock<ToolSchema> = LazyLock::new(|| {
        ToolSchema::internal(
            NAME,
            "Read an attachment in this conversation. attachment_id is the \"id\" listed in a \
             scitl:attachments block. The result has the same fields as that block. A text \
             attachment returns its text in \"content\", and an image is shown to you with the \
             result. Use this to look at an image whose \"delivered\" is \"name_only\", or at an \
             attachment whose content is no longer in the conversation. Other kinds of files \
             cannot be read.",
            json!({
                "type": "object",
                "properties": {
                    "attachment_id": { "type": "integer" }
                },
                "required": ["attachment_id"],
                "additionalProperties": false
            }),
        )
    });
    &SCHEMA
}

/// 渡し方は発言に付いた添付と同じく`attachments::delivery`で決める。読み込んだ中身は
/// このターンでモデルに渡すので、直近の発言の添付と同じ扱いにする。名前しか渡せない添付
/// (画像に対応しないモデルでの画像・その他の形式)は、読んでも何も増えないので失敗にする。
///
/// 中身(テキストの本文・画像)は往復で渡し、実行記録には名前などの情報だけを残す
/// ([`ToolOutput`])。実行記録は会話ログとして表示・エクスポートにも出るので、添付の本文を
/// 重ねて持たない。画像は実体のハッシュを[`ToolOutput::image_hashes`]に添えて返し、読み出しは
/// 呼び出し元に任せる。
pub fn execute(
    conn: &Connection,
    chat: Chat,
    image_input: bool,
    arguments: &Value,
) -> Result<ToolOutput> {
    let args = Args::parse(arguments, schema())?;
    let attachment = db_attachments::get_in_chat(conn, chat, args.required_i64("attachment_id")?)?;
    let view = &attachment.view;
    let delivered = attachments::delivery(view.kind, image_input, true);
    let (content, image_hashes) = match (delivered, &attachment.content) {
        (Delivery::Content, AttachmentContent::Text(text)) => (Some(text.as_str()), Vec::new()),
        (Delivery::Image, AttachmentContent::File { hash }) => (None, vec![hash.clone()]),
        _ if view.kind == AttachmentKind::Image => {
            return Err(CoreError::Attachment(
                "the current model does not accept images, so image attachments cannot be read"
                    .to_string(),
            ))
        }
        _ => {
            return Err(CoreError::Attachment(
                "only text and image attachments can be read".to_string(),
            ))
        }
    };
    let note = |content| {
        serde_json::to_value(AttachmentNote::new(view, delivered, content))
            .expect("an attachment note serializes to JSON")
    };
    Ok(ToolOutput {
        result: note(None),
        turn_result: content.map(|text| note(Some(text))),
        image_hashes,
    })
}

/// 実行記録に残した結果([`ToolOutput::result`])を、中身を伴わずに並べる形にする。記録は本文も
/// 画像も持たないので、渡し方を名前だけに直す。
pub(super) fn without_content(mut result: Value) -> Value {
    if let Some(delivered) = result.get_mut("delivered") {
        *delivered = json!(Delivery::NameOnly);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::attachments::NewAttachment;
    use crate::db::messages::{self, Kind, NewMessage, Origin, Role};

    struct Fixture {
        conn: Connection,
        chat: Chat,
    }

    impl Fixture {
        fn new() -> Self {
            let conn = db::open_in_memory().unwrap();
            let task_id = db::tasks::create_task(&conn).unwrap().id;
            Self {
                conn,
                chat: Chat::Task(task_id),
            }
        }

        fn attach(&self, kind: AttachmentKind, content: AttachmentContent) -> i64 {
            let message_id = messages::insert_message(
                &self.conn,
                NewMessage {
                    chat: self.chat,
                    role: Role::User,
                    content: "見て",
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    partial_reply: None,
                    reasoning: None,
                },
            )
            .unwrap();
            db_attachments::insert(
                &self.conn,
                message_id,
                &NewAttachment {
                    original_name: "file".to_string(),
                    mime_type: "application/octet-stream".to_string(),
                    kind,
                    size_bytes: 3,
                    content,
                },
            )
            .unwrap()
        }

        fn read(&self, id: i64, image_input: bool) -> Result<ToolOutput> {
            execute(
                &self.conn,
                self.chat,
                image_input,
                &json!({ "attachment_id": id }),
            )
        }

        fn image(&self) -> i64 {
            self.attach(
                AttachmentKind::Image,
                AttachmentContent::File {
                    hash: "a".repeat(64),
                },
            )
        }
    }

    #[test]
    fn returns_the_text_of_a_text_attachment() {
        let f = Fixture::new();
        let id = f.attach(
            AttachmentKind::Text,
            AttachmentContent::Text("本文".to_string()),
        );
        let output = f.read(id, false).unwrap();
        let turn_result = output.turn_result.unwrap();
        assert_eq!(turn_result["id"], id);
        assert_eq!(turn_result["delivered"], "content");
        assert_eq!(turn_result["content"], "本文");
        // 実行記録には本文を残さない。
        assert_eq!(output.result["id"], id);
        assert!(output.result.get("content").is_none());
        assert!(output.image_hashes.is_empty());
    }

    #[test]
    fn hands_the_image_over_for_a_model_that_accepts_images() {
        let f = Fixture::new();
        let id = f.image();
        let output = f.read(id, true).unwrap();
        assert_eq!(output.result["delivered"], "image");
        assert!(output.result.get("content").is_none());
        assert_eq!(output.image_hashes, ["a".repeat(64)]);
    }

    /// 記録に残した結果は、本文や画像を渡したと伝えたままにしない。
    #[test]
    fn a_recorded_result_placed_without_its_content_says_name_only() {
        let f = Fixture::new();
        let text = f.attach(
            AttachmentKind::Text,
            AttachmentContent::Text("本文".to_string()),
        );
        for (id, image_input) in [(text, false), (f.image(), true)] {
            let placed = without_content(f.read(id, image_input).unwrap().result);
            assert_eq!(placed["id"], id);
            assert_eq!(placed["delivered"], "name_only");
        }
    }

    #[test]
    fn refuses_what_it_can_only_name() {
        let f = Fixture::new();
        let image = f.image();
        let other = f.attach(
            AttachmentKind::Other,
            AttachmentContent::File {
                hash: "b".repeat(64),
            },
        );
        for (id, image_input) in [(image, false), (other, true)] {
            let err = f.read(id, image_input).err().unwrap();
            assert!(matches!(err, CoreError::Attachment(_)), "{err}");
        }
    }

    #[test]
    fn does_not_reach_attachments_of_another_chat() {
        let f = Fixture::new();
        let id = f.image();
        let err = execute(
            &f.conn,
            Chat::General,
            true,
            &json!({ "attachment_id": id }),
        )
        .err()
        .unwrap();
        assert!(matches!(err, CoreError::AttachmentNotFound(n) if n == id));
    }

    #[test]
    fn rejects_a_missing_or_non_integer_id() {
        let f = Fixture::new();
        for arguments in [json!({}), json!({ "attachment_id": "1" })] {
            let err = execute(&f.conn, f.chat, true, &arguments).err().unwrap();
            assert!(matches!(err, CoreError::InvalidArgument { .. }));
        }
    }
}
