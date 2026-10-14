use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use scitl_core::db::attachments::{
    self as db_attachments, AttachmentContent, AttachmentKind, NewAttachment,
};
use scitl_core::db::messages::{
    self, Chat, Kind, NewMessage, OperationSource, Origin, ReplyPart, Role,
};
use scitl_core::db::{self, memories, task_steps, tasks, Connection};
use scitl_core::in_flight::InFlightSet;
use scitl_core::orchestration::operations;
use scitl_core::tools;

use super::*;
use crate::{connect, Limits, Listener, LocalVersions};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture";

/// 資格情報ストアの代わり。
#[derive(Default)]
struct Registry(Mutex<std::collections::HashSet<String>>);

impl crate::DeviceRegistry for Registry {
    fn is_registered(&self, device_id: &str) -> Result<bool, scitl_core::CoreError> {
        Ok(self.0.lock().unwrap().contains(device_id))
    }

    fn register(&self, device_id: &str) -> Result<(), scitl_core::CoreError> {
        self.0.lock().unwrap().insert(device_id.to_string());
        Ok(())
    }
}

struct Device {
    ctx: Context,
    _dir: tempfile::TempDir,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let attachments =
            AttachmentStore::new(dir.path().join("blobs"), dir.path().join("revealed"));
        Self {
            ctx: Context {
                db: Arc::new(Mutex::new(db::open_in_memory().unwrap())),
                attachments,
                registry: Arc::new(Registry::default()),
            },
            _dir: dir,
        }
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.ctx.db.lock().unwrap()
    }

    fn device_id(&self) -> String {
        db::sync::device_state(&self.conn()).unwrap().device_id
    }

    /// 運ぶ形にした全部の行。2台を比べるのに使う(連番によらない)。
    fn transport(&self) -> Transport {
        let rows = db::sync::stamp_and_snapshot(&self.conn()).unwrap().rows;
        let (transport, invalid) = convert::to_transport(&rows);
        assert!(invalid.is_empty(), "{invalid:?}");
        transport
    }
}

fn limits() -> Limits {
    Limits {
        listen_window: Duration::from_secs(10),
        handshake: Duration::from_secs(5),
        connect: Duration::from_secs(5),
        idle: Duration::from_secs(10),
        max_message: 16 * 1024 * 1024,
    }
}

fn insert(
    conn: &Connection,
    chat: Chat,
    role: Role,
    kind: Kind,
    origin: Origin,
    content: &str,
    parts: Option<&[ReplyPart]>,
) -> i64 {
    messages::insert_message(
        conn,
        NewMessage {
            chat,
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
}

/// ひと通りの行を持つデータ。会話・ターンの実行記録(成功・失敗・外部ツール)・操作の記録・
/// 削除した行・添付(テキストと画像)・メモリ・総合チャット。
async fn populate(device: &Device) {
    let task_id = {
        let conn = device.conn();
        let task_id = tasks::create_task(&conn).unwrap().id;
        let steps: Vec<i64> = task_steps::add_steps(
            &conn,
            task_id,
            &["買い出し".to_string(), "調理".to_string()],
        )
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
        let chat = Chat::Task(task_id);
        insert(
            &conn,
            chat,
            Role::User,
            Kind::Normal,
            Origin::User,
            "夕飯を作る",
            None,
        );
        let turn = Origin::Turn {
            turn_id: "01JTURN0000000000000000000",
            attempt_no: 1,
        };
        let arguments = json!({ "step_id": steps[0], "done": true });
        let result = tools::execute(&conn, chat, true, tools::update_step::NAME, &arguments)
            .unwrap()
            .result;
        let record = json!({ "tool": "update_step", "arguments": arguments, "result": result, "call_id": "c1" });
        let record_id = insert(
            &conn,
            chat,
            Role::Tool,
            Kind::ToolExecution,
            turn,
            &record.to_string(),
            None,
        );
        let failed = json!({ "tool": "update_step", "arguments": { "step_id": 9999 }, "result": { "error": "not found" } });
        let failed_id = insert(
            &conn,
            chat,
            Role::Tool,
            Kind::ToolExecution,
            turn,
            &failed.to_string(),
            None,
        );
        let external =
            json!({ "tool": "search__find", "arguments": { "id": 5 }, "result": { "id": 7 } });
        let external_id = insert(
            &conn,
            chat,
            Role::Tool,
            Kind::ToolExecution,
            turn,
            &external.to_string(),
            None,
        );
        let parts = [
            ReplyPart::Reasoning {
                round: 1,
                text: "考える".to_string(),
            },
            ReplyPart::Tool {
                round: 1,
                record: record_id,
            },
            ReplyPart::Tool {
                round: 1,
                record: failed_id,
            },
            ReplyPart::Tool {
                round: 1,
                record: external_id,
            },
            ReplyPart::Text {
                round: 2,
                text: "買い出しを済ませました".to_string(),
            },
        ];
        insert(
            &conn,
            chat,
            Role::Assistant,
            Kind::Normal,
            turn,
            "",
            Some(&parts),
        );
        task_steps::delete_step(&conn, steps[1]).unwrap();

        let message_id = insert(
            &conn,
            Chat::General,
            Role::User,
            Kind::Normal,
            Origin::User,
            "見て",
            None,
        );
        db_attachments::insert(
            &conn,
            message_id,
            &NewAttachment {
                original_name: "memo.txt".to_string(),
                mime_type: "text/plain".to_string(),
                kind: AttachmentKind::Text,
                size_bytes: 3,
                content: AttachmentContent::Text("abc".to_string()),
            },
        )
        .unwrap();
        let hash = device.ctx.attachments.put(PNG).unwrap();
        let image_id = db_attachments::insert(
            &conn,
            message_id,
            &NewAttachment {
                original_name: "photo.png".to_string(),
                mime_type: "image/png".to_string(),
                kind: AttachmentKind::Image,
                size_bytes: PNG.len() as i64,
                content: AttachmentContent::File { hash },
            },
        )
        .unwrap();
        let read = json!({ "tool": "read_attachment", "arguments": { "attachment_id": image_id },
                           "result": { "id": image_id, "name": "photo.png" } });
        let turn = Origin::Turn {
            turn_id: "01JTURN0000000000000000001",
            attempt_no: 1,
        };
        let read_id = insert(
            &conn,
            Chat::General,
            Role::Tool,
            Kind::ToolExecution,
            turn,
            &read.to_string(),
            None,
        );
        insert(
            &conn,
            Chat::General,
            Role::Assistant,
            Kind::Normal,
            turn,
            "",
            Some(&[
                ReplyPart::Tool {
                    round: 1,
                    record: read_id,
                },
                ReplyPart::Text {
                    round: 1,
                    text: "猫".to_string(),
                },
            ]),
        );

        let ids: Vec<i64> = memories::add(&conn, &["朝型".to_string(), "猫が好き".to_string()])
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        memories::delete(&conn, ids[1]).unwrap();
        let gone = tasks::create_task(&conn).unwrap().id;
        tasks::delete_task(&conn, gone).unwrap();
        task_id
    };
    operations::rename_task(
        device.ctx.db.clone(),
        &InFlightSet::new(),
        OperationSource::Ui,
        task_id,
        "夕飯".to_string(),
    )
    .await
    .unwrap();
}

/// 2台をループバックでつなぎ、それぞれの側で移送する。
async fn sync(
    listener: &Device,
    connector: &Device,
) -> (Result<Outcome, SyncError>, Result<Outcome, SyncError>) {
    let local = LocalVersions {
        db_version: db::schema_version(),
    };
    let bound = Listener::bind(&[IpAddr::V4(Ipv4Addr::LOCALHOST)], limits())
        .await
        .unwrap();
    let addr = bound.addresses()[0];
    let code = bound.code().clone();
    let (a_id, b_id) = (listener.device_id(), connector.device_id());
    let a = async {
        let mut session = bound.accept(local).await?;
        copy(&mut session, &listener.ctx, Side::Listener, &a_id).await
    };
    let b = async {
        let mut session = connect(&[addr], &code, local, limits()).await?;
        copy(&mut session, &connector.ctx, Side::Connector, &b_id).await
    };
    tokio::join!(a, b)
}

fn marks(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT (SELECT count(*) FROM tasks WHERE sync_changed = 1)
              + (SELECT count(*) FROM task_steps WHERE sync_changed = 1)
              + (SELECT count(*) FROM memories WHERE sync_changed = 1)
              + (SELECT count(*) FROM messages WHERE sync_changed = 1)",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn everything_is_copied_to_the_empty_device_either_way() {
    for data_on_listener in [true, false] {
        let (a, b) = (Device::new(), Device::new());
        let (from, to) = if data_on_listener { (&a, &b) } else { (&b, &a) };
        populate(from).await;
        let (ra, rb) = sync(&a, &b).await;
        let (sent, received) = if data_on_listener { (ra, rb) } else { (rb, ra) };
        let Outcome::Sent(sent) = sent.unwrap() else {
            panic!("not sent")
        };
        let Outcome::Received(received) = received.unwrap() else {
            panic!("not received")
        };
        assert_eq!(sent, received);
        assert_eq!(received.files, 1);
        assert_eq!(
            (received.tasks, received.memories, received.attachments),
            (2, 2, 2)
        );

        // 連番によらない形で同じ。版も、実行記録の中のIDの指す先も揃う。
        assert_eq!(marks(&to.conn()), 0);
        assert_eq!(to.transport(), from.transport());
        assert!(to
            .ctx
            .attachments
            .contains(&from.transport().attachments[1].file_hash.clone().unwrap())
            .unwrap());

        // 受け取った側でも、表示とモデルへの履歴は送った側と同じ並び。
        let task_of = |d: &Device| tasks::list_summaries(&d.conn()).unwrap()[0].id;
        let view = |d: &Device, chat: Chat| -> Vec<(String, String)> {
            messages::list_for_chat(&d.conn(), chat)
                .unwrap()
                .into_iter()
                .map(|m| (m.role.as_str().to_string(), m.content))
                .collect()
        };
        assert_eq!(
            view(to, Chat::Task(task_of(to))).len(),
            view(from, Chat::Task(task_of(from))).len()
        );
        assert_eq!(view(to, Chat::General), view(from, Chat::General));

        // 両側にカーソル。受け取った側は、写した側のカウンターまでを受け取り、自分の行は渡し済み。
        let (from_id, to_id) = (from.device_id(), to.device_id());
        let (to_sent, to_received) = db::sync::cursor(&to.conn(), &from_id).unwrap().unwrap();
        let (from_sent, from_received) = db::sync::cursor(&from.conn(), &to_id).unwrap().unwrap();
        assert_eq!((from_sent, from_received), (to_received, to_sent));
    }
}

#[tokio::test]
async fn ids_inside_tool_records_point_at_the_copied_rows() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let (ra, rb) = sync(&a, &b).await;
    ra.unwrap();
    rb.unwrap();

    let conn = b.conn();
    let record: Value = conn
        .query_row(
            "SELECT content FROM messages WHERE content LIKE '%update_step%' AND content NOT LIKE '%error%'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap();
    let step_id = record["arguments"]["step_id"].as_i64().unwrap();
    let step_uid: String = conn
        .query_row("SELECT uid FROM task_steps WHERE id = ?1", [step_id], |r| {
            r.get(0)
        })
        .unwrap();
    let source_uid: String = a
        .conn()
        .query_row(
            "SELECT uid FROM task_steps WHERE description = '買い出し'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(step_uid, source_uid);
    // 失敗の記録と外部ツールの記録は、そのまま運ぶ。
    for kept in [
        r#""step_id":9999"#,
        r#""tool":"search__find","arguments":{"id":5}"#,
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages WHERE instr(content, ?1) > 0",
                [kept],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{kept}");
    }
}

#[tokio::test]
async fn copying_is_refused_unless_exactly_one_side_has_data() {
    let (a, b) = (Device::new(), Device::new());
    let (ra, rb) = sync(&a, &b).await;
    assert!(matches!(ra, Err(SyncError::NothingToCopy)), "{ra:?}");
    assert!(matches!(rb, Err(SyncError::NothingToCopy)), "{rb:?}");

    populate(&a).await;
    memories::add(&b.conn(), &["夜型".to_string()]).unwrap();
    let (ra, rb) = sync(&a, &b).await;
    assert!(matches!(ra, Err(SyncError::BothHaveData)), "{ra:?}");
    assert!(matches!(rb, Err(SyncError::BothHaveData)), "{rb:?}");
}

#[tokio::test]
async fn copies_of_the_same_database_are_refused() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let id = a.device_id();
    b.conn()
        .execute("UPDATE sync_state SET device_id = ?1", [&id])
        .unwrap();
    let (ra, rb) = sync(&a, &b).await;
    assert!(matches!(ra, Err(SyncError::SameDevice)), "{ra:?}");
    assert!(matches!(rb, Err(SyncError::SameDevice)), "{rb:?}");
}

#[tokio::test]
async fn rows_out_of_shape_are_shown_on_the_sending_side_and_nothing_is_written() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let bad_id: i64 = {
        let conn = a.conn();
        let id = tasks::create_task(&conn).unwrap().id;
        conn.execute("UPDATE tasks SET title = ' 余白' WHERE id = ?1", [id])
            .unwrap();
        let unknown = json!({ "tool": "old_tool", "arguments": {}, "result": {} });
        let chat = Chat::Task(id);
        insert(
            &conn,
            chat,
            Role::Tool,
            Kind::ToolExecution,
            Origin::Operation(OperationSource::Cli),
            &unknown.to_string(),
            None,
        );
        id
    };
    let (ra, rb) = sync(&a, &b).await;
    let Err(SyncError::InvalidRows(rows)) = ra else {
        panic!("{ra:?}")
    };
    assert!(
        rows.iter()
            .any(|r| r.table == "tasks" && r.id == Some(bad_id)),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.table == "messages" && r.reason.contains("old_tool")),
        "{rows:?}"
    );
    assert!(
        matches!(rb, Err(SyncError::PeerRefused(AbortReason::InvalidRows))),
        "{rb:?}"
    );
    assert!(!db::sync::has_data(&b.conn()).unwrap());
}

#[tokio::test]
async fn files_the_receiver_already_has_are_not_sent_again() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    b.ctx.attachments.put(PNG).unwrap();
    let (ra, rb) = sync(&a, &b).await;
    let Outcome::Received(counts) = rb.unwrap() else {
        panic!()
    };
    assert_eq!(counts.files, 0);
    ra.unwrap();
}

#[test]
fn rows_from_the_peer_are_checked_like_the_apps_own() {
    let device = Device::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(populate(&device));
    let good = device.transport();
    validate::check(&good).unwrap();

    let mut bad = good.clone();
    bad.memories[0].content = "朝\n型".to_string();
    bad.steps[0].task = "f".repeat(32);
    bad.messages[0].created_at = "yesterday".to_string();
    bad.attachments[0].size_bytes = 99;
    bad.tasks[1].uid = bad.tasks[0].uid.clone();
    let rows = validate::check(&bad).unwrap_err();
    for table in ["memories", "task_steps", "messages", "attachments", "tasks"] {
        assert!(rows.iter().any(|r| r.table == table), "{table}: {rows:?}");
    }

    // 置き換えていないIDの残った記録は、受け取る側で断る。
    let mut unreplaced = good.clone();
    let record = unreplaced
        .messages
        .iter_mut()
        .find(|m| m.content.contains("\"update_step\"") && !m.content.contains("error"))
        .unwrap();
    let mut content: Value = serde_json::from_str(&record.content).unwrap();
    content["arguments"]["step_id"] = json!(1);
    record.content = content.to_string();
    let rows = validate::check(&unreplaced).unwrap_err();
    assert!(rows[0].reason.contains("not replaced"), "{rows:?}");
}

#[tokio::test]
async fn files_out_of_shape_are_shown_on_the_sending_side() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let image_id: i64 = a
        .conn()
        .query_row("SELECT id FROM attachments WHERE kind = 'image'", [], |r| {
            r.get(0)
        })
        .unwrap();
    a.conn()
        .execute(
            "UPDATE attachments SET size_bytes = 1 WHERE id = ?1",
            [image_id],
        )
        .unwrap();
    let (ra, rb) = sync(&a, &b).await;
    let Err(SyncError::InvalidRows(rows)) = ra else {
        panic!("{ra:?}")
    };
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].table, rows[0].id), ("attachments", Some(image_id)));
    assert!(
        matches!(rb, Err(SyncError::PeerRefused(AbortReason::InvalidRows))),
        "{rb:?}"
    );
}

/// 戻したDB(同じ端末IDで番号が巻き戻ったもの)が、戻す前に渡した行を受け取ると、端末IDを作り直す。
#[tokio::test]
async fn a_rolled_back_receiver_gets_a_new_device_id() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let restored = Device::new();
    let id = a.device_id();
    // aの端末IDで振った版を持つ行を、bを経て空に戻したaへ写す。
    let (ra, rb) = sync(&a, &b).await;
    ra.unwrap();
    rb.unwrap();
    restored
        .conn()
        .execute("UPDATE sync_state SET device_id = ?1", [&id])
        .unwrap();
    let (ra, rb) = sync(&b, &restored).await;
    ra.unwrap();
    rb.unwrap();
    assert_ne!(restored.device_id(), id);
}

#[test]
fn links_must_point_back_into_the_same_chat() {
    let device = Device::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(populate(&device));
    let mut bad = device.transport();
    let last = bad.messages.len() - 1;
    let later = bad.messages[last].uid.clone();
    let first_normal = bad
        .messages
        .iter_mut()
        .find(|m| m.kind == "normal")
        .unwrap();
    first_normal.prev_uid = Some(later);
    let rows = validate::check(&bad).unwrap_err();
    assert!(rows[0].reason.contains("prev_uid"), "{rows:?}");
}
