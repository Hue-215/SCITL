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
use crate::conflict::ChosenSide;
use crate::write_plan::WriteCounts;
use crate::{connect, Limits, Listener, LocalVersions};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture";

/// 資格情報ストアの代わり。
#[derive(Default)]
struct Registry(Mutex<std::collections::HashMap<String, String>>);

impl crate::DeviceRegistry for Registry {
    fn is_current(&self, device_id: &str, token: &str) -> Result<bool, scitl_core::CoreError> {
        Ok(self.0.lock().unwrap().get(device_id).map(String::as_str) == Some(token))
    }

    fn register(&self, device_id: &str, token: &str) -> Result<(), scitl_core::CoreError> {
        self.0
            .lock()
            .unwrap()
            .insert(device_id.to_string(), token.to_string());
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

    /// 運ぶ形にした全部の行。2台を比べるのに使う(連番にも、試行の番号にもよらない)。
    fn transport(&self) -> Transport {
        let (mut transport, invalid) =
            db_sync::transaction(&self.conn(), |tx| convert::read(tx, Filter::All)).unwrap();
        assert!(invalid.is_empty(), "{invalid:?}");
        for m in &mut transport.messages {
            m.attempt = None;
        }
        transport
    }

    fn task_id(&self, uid: &str) -> i64 {
        self.conn()
            .query_row("SELECT id FROM tasks WHERE uid = ?1", [uid], |r| r.get(0))
            .unwrap()
    }

    fn task_uid(&self, title: &str) -> String {
        self.conn()
            .query_row("SELECT uid FROM tasks WHERE title = ?1", [title], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn cursor(&self, peer: &Device) -> Cursor {
        db_sync::cursor(&self.conn(), &peer.device_id()).unwrap()
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

/// 競合で、すべての単位に同じ側を選ぶ。見せられた競合を覚える。
struct Choose {
    side: ChosenSide,
    seen: Mutex<Vec<ConflictView>>,
}

impl Choose {
    fn new(side: ChosenSide) -> Self {
        Self {
            side,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<ConflictView> {
        self.seen.lock().unwrap().clone()
    }
}

impl Resolver for Choose {
    fn choose(&self, conflicts: Vec<ConflictView>) -> BoxFuture<'_, Option<Vec<ConflictChoice>>> {
        let choices = conflicts
            .iter()
            .map(|c| ConflictChoice {
                id: c.id.clone(),
                side: self.side,
            })
            .collect();
        self.seen.lock().unwrap().extend(conflicts);
        Box::pin(async move { Some(choices) })
    }
}

/// 競合が出たら失敗にする。
struct NoConflicts;

impl Resolver for NoConflicts {
    fn choose(&self, conflicts: Vec<ConflictView>) -> BoxFuture<'_, Option<Vec<ConflictChoice>>> {
        panic!("unexpected conflicts: {conflicts:?}")
    }
}

/// 確認では書き込む。
struct AutoWrite;

impl Confirmer for AutoWrite {
    fn confirm(&self, _: WritePlan) -> BoxFuture<'_, Option<WriteAnswer>> {
        Box::pin(async { Some(WriteAnswer::Write) })
    }
}

/// 確認の答えを操作・計画を覚える。
struct Confirming {
    /// 最初に返す答え。そのあとは書き込む。
    first: WriteAnswer,
    seen: Mutex<Vec<WritePlan>>,
}

impl Confirmer for Confirming {
    fn confirm(&self, plan: WritePlan) -> BoxFuture<'_, Option<WriteAnswer>> {
        let seen = self.seen.lock().unwrap().len();
        if seen == 0 {
            self.seen.lock().unwrap().push(plan);
            let first = self.first;
            Box::pin(async move { Some(first) })
        } else {
            self.seen.lock().unwrap().push(plan);
            Box::pin(async { Some(WriteAnswer::Write) })
        }
    }
}

/// 2台をループバックでつなぎ、それぞれの側で同期する。`listener`の側で競合を`resolver`で選ぶ。
/// `on_connector_stage`は、つなぐ側の段ごとに呼ぶ(途中で書き込みを挟む試験に使う)。
async fn sync_with(
    listener: &Device,
    connector: &Device,
    resolver: &dyn Resolver,
    on_connector_stage: impl Fn(Stage) + Sync,
) -> (Result<Outcome, SyncError>, Result<Outcome, SyncError>) {
    sync_with_stages(listener, connector, resolver, |_| {}, on_connector_stage).await
}

async fn sync_with_stages(
    listener: &Device,
    connector: &Device,
    resolver: &dyn Resolver,
    on_listener_stage: impl Fn(Stage) + Sync,
    on_connector_stage: impl Fn(Stage) + Sync,
) -> (Result<Outcome, SyncError>, Result<Outcome, SyncError>) {
    sync_with_confirmed(
        listener,
        connector,
        resolver,
        &AutoWrite,
        on_listener_stage,
        on_connector_stage,
    )
    .await
}

async fn sync_with_confirmed(
    listener: &Device,
    connector: &Device,
    resolver: &dyn Resolver,
    confirmer: &dyn Confirmer,
    on_listener_stage: impl Fn(Stage) + Sync,
    on_connector_stage: impl Fn(Stage) + Sync,
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
    let (a_set, b_set) = (InFlightSet::<()>::new(), InFlightSet::<()>::new());
    let (a_stop, b_stop) = (
        a_set.try_begin_all().unwrap(),
        b_set.try_begin_all().unwrap(),
    );
    let a = async {
        let mut session = bound.accept(local).await?;
        let result = super::sync(
            &mut session,
            &listener.ctx,
            &a_id,
            a_stop.stop_signal(),
            on_listener_stage,
            resolver,
            confirmer,
        )
        .await;
        session.close().await;
        result
    };
    let b = async {
        let mut session = connect(&[addr], &code, local, limits()).await?;
        let result = super::sync(
            &mut session,
            &connector.ctx,
            &b_id,
            b_stop.stop_signal(),
            on_connector_stage,
            &NoConflicts,
            &AutoWrite,
        )
        .await;
        session.close().await;
        result
    };
    tokio::join!(a, b)
}

async fn sync(listener: &Device, connector: &Device) -> (Outcome, Outcome) {
    let (ra, rb) = sync_with(listener, connector, &NoConflicts, |_| {}).await;
    (ra.unwrap(), rb.unwrap())
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

/// 2台が同じ内容になったか(連番・試行の番号によらない)。違えば、違う行だけを示す。
fn assert_same(a: &Device, b: &Device) {
    fn diff<R: std::fmt::Debug + PartialEq>(
        table: &str,
        a: &[R],
        b: &[R],
        uid: impl Fn(&R) -> &str,
        out: &mut Vec<String>,
    ) {
        let bs: HashMap<&str, &R> = b.iter().map(|r| (uid(r), r)).collect();
        let as_: HashMap<&str, &R> = a.iter().map(|r| (uid(r), r)).collect();
        for r in a {
            match bs.get(uid(r)) {
                Some(o) if *o == r => {}
                o => out.push(format!("{table}: {r:?}\n   vs {o:?}")),
            }
        }
        for r in b.iter().filter(|r| !as_.contains_key(uid(r))) {
            out.push(format!("{table}: only on b {r:?}"));
        }
    }
    let (ta, tb) = (a.transport(), b.transport());
    let mut out = Vec::new();
    diff("tasks", &ta.tasks, &tb.tasks, |r| &r.uid, &mut out);
    diff("steps", &ta.steps, &tb.steps, |r| &r.uid, &mut out);
    diff("memories", &ta.memories, &tb.memories, |r| &r.uid, &mut out);
    diff("messages", &ta.messages, &tb.messages, |r| &r.uid, &mut out);
    diff(
        "attachments",
        &ta.attachments,
        &tb.attachments,
        |r| &r.uid,
        &mut out,
    );
    assert!(out.is_empty(), "{}", out.join("\n"));
    assert_eq!(marks(&a.conn()), 0);
    assert_eq!(marks(&b.conn()), 0);
}

fn alive_texts(device: &Device, chat: Chat) -> Vec<String> {
    let conn = device.conn();
    let rows = messages::list_for_chat(&conn, chat).unwrap();
    rows.into_iter()
        .filter(|m| m.kind == Kind::Normal)
        .map(|m| {
            match m.parts.iter().find_map(|p| match p {
                ReplyPart::Text { text, .. } => Some(text.clone()),
                _ => None,
            }) {
                Some(text) => text,
                None => m.content,
            }
        })
        .collect()
}

fn say(device: &Device, chat: Chat, text: &str) {
    insert(
        &device.conn(),
        chat,
        Role::User,
        Kind::Normal,
        Origin::User,
        text,
        None,
    );
}

/// 1つのターンの返信だけを書く(実行記録なし)。
fn reply(device: &Device, chat: Chat, turn_id: &str, attempt_no: i64, text: &str) -> i64 {
    insert(
        &device.conn(),
        chat,
        Role::Assistant,
        Kind::Normal,
        Origin::Turn {
            turn_id,
            attempt_no,
        },
        "",
        Some(&[ReplyPart::Text {
            round: 1,
            text: text.to_string(),
        }]),
    )
}

#[tokio::test]
async fn everything_reaches_the_empty_device_either_way() {
    for data_on_listener in [true, false] {
        let (a, b) = (Device::new(), Device::new());
        let (from, to) = if data_on_listener { (&a, &b) } else { (&b, &a) };
        populate(from).await;
        let (ra, rb) = sync(&a, &b).await;
        let (sent, received) = if data_on_listener { (ra, rb) } else { (rb, ra) };
        assert_eq!(sent.changed, Counts::default(), "{data_on_listener}");
        assert_eq!(received.changed.files, 1);
        assert_eq!(
            (
                received.changed.tasks,
                received.changed.conversations,
                received.changed.memories,
                received.changed.attachments
            ),
            (2, 2, 2, 2)
        );

        // 連番によらない形で同じ。版も、実行記録の中のIDの指す先も揃う。
        assert_same(from, to);
        assert!(to
            .ctx
            .attachments
            .contains(&from.transport().attachments[1].file_hash.clone().unwrap())
            .unwrap());

        // 受け取った側でも、表示とモデルへの履歴は送った側と同じ並び。
        let task_of = |d: &Device| tasks::list_summaries(&d.conn()).unwrap()[0].id;
        assert_eq!(
            alive_texts(to, Chat::Task(task_of(to))),
            alive_texts(from, Chat::Task(task_of(from)))
        );
        assert_eq!(
            alive_texts(to, Chat::General),
            alive_texts(from, Chat::General)
        );

        // 両側にカーソル。互いの「渡した位置」と「受け取った位置」が合う。
        let (to_cursor, from_cursor) = (to.cursor(from), from.cursor(to));
        assert_eq!(from_cursor.sent_seq, to_cursor.received_seq);
        assert_eq!(from_cursor.received_seq, to_cursor.sent_seq);

        // もう一度同期しても何も変わらない。
        let (ra, rb) = sync(&a, &b).await;
        assert_eq!(
            (ra.changed, rb.changed),
            (Counts::default(), Counts::default())
        );
        assert_same(&a, &b);
    }
}

#[tokio::test]
async fn ids_inside_tool_records_point_at_the_copied_rows() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;

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
async fn two_empty_devices_sync_nothing() {
    let (a, b) = (Device::new(), Device::new());
    let (ra, rb) = sync(&a, &b).await;
    assert_eq!(
        (ra.changed, rb.changed),
        (Counts::default(), Counts::default())
    );
}

#[tokio::test]
async fn copies_of_the_same_database_are_refused() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let id = a.device_id();
    b.conn()
        .execute("UPDATE sync_state SET device_id = ?1", [&id])
        .unwrap();
    let (ra, rb) = sync_with(&a, &b, &NoConflicts, |_| {}).await;
    assert!(matches!(ra, Err(SyncError::SameDevice)), "{ra:?}");
    assert!(matches!(rb, Err(SyncError::SameDevice)), "{rb:?}");
}

#[tokio::test]
async fn rows_out_of_shape_are_shown_on_their_own_device_and_nothing_is_written() {
    for bad_on_listener in [true, false] {
        let (a, b) = (Device::new(), Device::new());
        let (bad, good) = if bad_on_listener { (&a, &b) } else { (&b, &a) };
        populate(bad).await;
        let bad_id: i64 = {
            let conn = bad.conn();
            let id = tasks::create_task(&conn).unwrap().id;
            conn.execute("UPDATE tasks SET title = ' 余白' WHERE id = ?1", [id])
                .unwrap();
            let unknown = json!({ "tool": "old_tool", "arguments": {}, "result": {} });
            insert(
                &conn,
                Chat::Task(id),
                Role::Tool,
                Kind::ToolExecution,
                Origin::Operation(OperationSource::Cli),
                &unknown.to_string(),
                None,
            );
            id
        };
        let (ra, rb) = sync_with(&a, &b, &NoConflicts, |_| {}).await;
        let (here, there) = if bad_on_listener { (ra, rb) } else { (rb, ra) };
        let Err(SyncError::InvalidRows(rows)) = here else {
            panic!("{here:?}")
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
            matches!(there, Err(SyncError::PeerRefused(AbortReason::InvalidRows))),
            "{there:?}"
        );
        assert!(good.transport().tasks.is_empty());
    }
}

#[tokio::test]
async fn files_the_receiver_already_has_are_not_sent_again() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    b.ctx.attachments.put(PNG).unwrap();
    let (_, rb) = sync(&a, &b).await;
    assert_eq!(rb.changed.files, 0);
}

#[test]
fn rows_from_the_peer_are_checked_like_the_apps_own() {
    let device = Device::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(populate(&device));
    let (good, _) =
        db_sync::transaction(&device.conn(), |tx| convert::read(tx, Filter::All)).unwrap();
    let nothing = Known::default();
    validate::check(&good, &nothing).unwrap();

    let mut bad = good.clone();
    bad.memories[0].content = "朝\n型".to_string();
    bad.steps[0].task = "f".repeat(32);
    bad.messages[0].created_at = "yesterday".to_string();
    bad.attachments[0].size_bytes = 99;
    bad.tasks[1].uid = bad.tasks[0].uid.clone();
    let rows = validate::check(&bad, &nothing).unwrap_err();
    for table in ["memories", "task_steps", "messages", "attachments", "tasks"] {
        assert!(rows.iter().any(|r| r.table == table), "{table}: {rows:?}");
    }

    // 本体が受け付けない、名前の無い添付。
    let mut nameless = good.clone();
    nameless.attachments[0].original_name = " ".to_string();
    let rows = validate::check(&nameless, &nothing).unwrap_err();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].reason.contains("name"), "{rows:?}");

    // エクスポートは`mime_type`を中身から決めた値とみなして書くので、本体が付けない値を入れない。
    let mut disguised = good.clone();
    disguised.attachments[0].mime_type = "x)\n\n<img src=\"https://e.example/\">".to_string();
    let rows = validate::check(&disguised, &nothing).unwrap_err();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].reason.contains("MIME"), "{rows:?}");

    let mut unknown = good.clone();
    unknown.attachments[0].kind = "video".to_string();
    let rows = validate::check(&unknown, &nothing).unwrap_err();
    assert!(rows[0].reason.contains("unknown kind"), "{rows:?}");

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
    let rows = validate::check(&unreplaced, &nothing).unwrap_err();
    assert!(rows[0].reason.contains("not replaced"), "{rows:?}");
}

#[test]
fn links_may_point_at_rows_the_receiver_already_has() {
    let device = Device::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(populate(&device));
    let (all, _) =
        db_sync::transaction(&device.conn(), |tx| convert::read(tx, Filter::All)).unwrap();
    let known = db_sync::transaction(&device.conn(), Known::load).unwrap();
    let only_replies = Transport {
        messages: all
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .cloned()
            .collect(),
        ..Transport::default()
    };
    validate::check(&only_replies, &known).unwrap();
    assert!(validate::check(&only_replies, &Known::default()).is_err());
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
    let (ra, rb) = sync_with(&a, &b, &NoConflicts, |_| {}).await;
    let Err(SyncError::InvalidRows(rows)) = ra else {
        panic!("{ra:?}")
    };
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].table, "attachments");
    assert!(
        matches!(rb, Err(SyncError::PeerRefused(AbortReason::InvalidRows))),
        "{rb:?}"
    );
}

/// 戻したDB(同じ端末IDで番号が巻き戻ったもの)は、相手の持つ版で分かり、端末IDを作り直して
/// 全部をやり取りし直す。
#[tokio::test]
async fn a_rolled_back_device_gets_a_new_device_id() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let restored = Device::new();
    let id = a.device_id();
    restored
        .conn()
        .execute("UPDATE sync_state SET device_id = ?1", [&id])
        .unwrap();
    for restored_listens in [true, false] {
        if restored_listens {
            sync(&restored, &b).await;
        } else {
            sync(&b, &restored).await;
        }
        assert_ne!(restored.device_id(), id);
        assert_same(&restored, &b);
    }
}

#[tokio::test]
async fn a_stop_request_before_writing_ends_as_cancelled() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let local = LocalVersions {
        db_version: db::schema_version(),
    };
    let bound = Listener::bind(&[IpAddr::V4(Ipv4Addr::LOCALHOST)], limits())
        .await
        .unwrap();
    let (addr, code) = (bound.addresses()[0], bound.code().clone());
    let (a_id, b_id) = (a.device_id(), b.device_id());
    let (a_set, b_set) = (InFlightSet::<()>::new(), InFlightSet::<()>::new());
    let (a_stop, b_stop) = (
        a_set.try_begin_all().unwrap(),
        b_set.try_begin_all().unwrap(),
    );
    // 待ち受ける側(a)は、始める前に止める指示を受けている。
    a_set.request_stop_all();
    let listener = async {
        let mut session = bound.accept(local).await?;
        super::sync(
            &mut session,
            &a.ctx,
            &a_id,
            a_stop.stop_signal(),
            |_| {},
            &NoConflicts,
            &AutoWrite,
        )
        .await
    };
    let connector = async {
        let mut session = connect(&[addr], &code, local, limits()).await?;
        super::sync(
            &mut session,
            &b.ctx,
            &b_id,
            b_stop.stop_signal(),
            |_| {},
            &NoConflicts,
            &AutoWrite,
        )
        .await
    };
    let (ra, rb) = tokio::join!(listener, connector);
    assert!(matches!(ra, Err(SyncError::Cancelled)), "{ra:?}");
    assert!(rb.is_err(), "{rb:?}");
    assert!(b.transport().tasks.is_empty());
}

/// 結合後のタスクの全体を、操作の記録として会話の末尾に置く。
#[tokio::test]
async fn edits_to_different_rows_of_a_task_are_merged() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    let (a_task, b_task) = (a.task_id(&task), b.task_id(&task));
    task_steps::add_steps(&a.conn(), a_task, &["片付け".to_string()]).unwrap();
    b.conn()
        .execute(
            "UPDATE tasks SET deadline = '2026-10-31' WHERE id = ?1",
            [b_task],
        )
        .unwrap();
    for listener_is_a in [true, false] {
        if listener_is_a {
            sync(&a, &b).await;
        } else {
            sync(&b, &a).await;
        }
        assert_same(&a, &b);
    }
    let detail = |d: &Device, id: i64| {
        let conn = d.conn();
        let steps: Vec<String> = task_steps::list_for_task(&conn, id)
            .unwrap()
            .into_iter()
            .map(|s| s.description)
            .collect();
        (tasks::get_task(&conn, id).unwrap().deadline, steps)
    };
    assert_eq!(
        detail(&a, a_task),
        (
            Some("2026-10-31".to_string()),
            vec!["買い出し".to_string(), "片付け".to_string()]
        )
    );
    // 記録は1つだけ(2回目の同期では、どちらも相手の変更を知っている)。
    let records: Vec<String> = a
        .conn()
        .prepare("SELECT content FROM messages WHERE source = 'sync' AND task_id = ?1")
        .unwrap()
        .query_map([a_task], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    let record: Value = serde_json::from_str(&records[0]).unwrap();
    assert_eq!(record["tool"], "get_current_task_detail");
    assert_eq!(record["result"]["task"]["deadline"], "2026-10-31");
    assert_eq!(record["result"]["steps"].as_array().unwrap().len(), 2);
}

/// その側の記録が相手の会話に付く。
#[tokio::test]
async fn edits_on_one_side_are_taken_without_a_record() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    b.conn()
        .execute("UPDATE tasks SET title = '夕食' WHERE uid = ?1", [&task])
        .unwrap();
    sync(&a, &b).await;
    assert_same(&a, &b);
    assert_eq!(a.task_uid("夕食"), task);
    let records: i64 = a
        .conn()
        .query_row(
            "SELECT count(*) FROM messages WHERE source = 'sync'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(records, 0);
}

/// タスクを丸ごと選ばせる。選ばなかった側にしか無い行は消える。
#[tokio::test]
async fn the_same_row_changed_on_both_sides_is_chosen_by_the_user() {
    for side in [ChosenSide::Ours, ChosenSide::Theirs] {
        let (a, b) = (Device::new(), Device::new());
        populate(&a).await;
        sync(&a, &b).await;
        let task = a.task_uid("夕飯");
        let (a_task, b_task) = (a.task_id(&task), b.task_id(&task));
        a.conn()
            .execute("UPDATE tasks SET title = '晩ご飯' WHERE id = ?1", [a_task])
            .unwrap();
        task_steps::add_steps(&a.conn(), a_task, &["皿洗い".to_string()]).unwrap();
        b.conn()
            .execute("UPDATE tasks SET title = '夕食' WHERE id = ?1", [b_task])
            .unwrap();
        task_steps::add_steps(&b.conn(), b_task, &["盛り付け".to_string()]).unwrap();

        let choose = Choose::new(side);
        let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
        assert_eq!(ra.unwrap().conflicts, 1);
        rb.unwrap();
        let seen = choose.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].kind, crate::ConflictKind::Task);
        assert_eq!(seen[0].ours.title.as_deref(), Some("晩ご飯"));
        assert_eq!(seen[0].theirs.title.as_deref(), Some("夕食"));
        assert_same(&a, &b);

        let (title, step) = match side {
            ChosenSide::Ours => ("晩ご飯", "皿洗い"),
            ChosenSide::Theirs => ("夕食", "盛り付け"),
        };
        let steps: Vec<String> = task_steps::list_for_task(&a.conn(), a_task)
            .unwrap()
            .into_iter()
            .map(|s| s.description)
            .collect();
        assert_eq!(steps, ["買い出し", step]);
        assert_eq!(a.task_uid(title), task);

        // 選んだあとは、どちらで同期しても聞き直さない。
        sync(&b, &a).await;
        assert_same(&a, &b);
    }
}

#[tokio::test]
async fn a_chat_continued_on_both_sides_is_chosen_by_the_user() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    let (a_chat, b_chat) = (Chat::Task(a.task_id(&task)), Chat::Task(b.task_id(&task)));
    say(&a, a_chat, "Aで続ける");
    reply(&a, a_chat, "01JTURNA000000000000000000", 1, "Aの返事");
    say(&b, b_chat, "Bで続ける");
    reply(&b, b_chat, "01JTURNB000000000000000000", 1, "Bの返事");

    let choose = Choose::new(ChosenSide::Theirs);
    let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
    assert_eq!(ra.unwrap().conflicts, 1);
    rb.unwrap();
    assert_eq!(choose.seen()[0].ours.only_here, 2);
    assert_same(&a, &b);
    let texts = alive_texts(&a, a_chat);
    assert_eq!(&texts[texts.len() - 2..], ["Bで続ける", "Bの返事"]);
    assert!(!texts.contains(&"Aで続ける".to_string()));
    assert_eq!(alive_texts(&b, b_chat), texts);
}

#[tokio::test]
async fn a_chat_continued_on_one_side_is_taken() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    say(&b, Chat::General, "続き");
    sync(&a, &b).await;
    assert_same(&a, &b);
    assert_eq!(alive_texts(&a, Chat::General).last().unwrap(), "続き");
}

/// 黙ってタスクごと見えなくしない。
#[tokio::test]
async fn deleting_a_task_the_other_side_kept_using_is_chosen_by_the_user() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    let a_task = a.task_id(&task);
    tasks::delete_task(&a.conn(), a_task).unwrap();
    say(&b, Chat::Task(b.task_id(&task)), "まだ使う");

    let choose = Choose::new(ChosenSide::Theirs);
    let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
    assert_eq!(ra.unwrap().conflicts, 1);
    rb.unwrap();
    assert!(choose.seen()[0].ours.deleted);
    assert_same(&a, &b);
    assert!(tasks::get_task(&a.conn(), a_task).is_ok());
}

/// 初めて同期する2台は、どちらも総合チャットを使っていれば必ず選ばせる(「会話の枝と、競合の単位」)。タスクは別々なので
/// 両方が残り、同じ本文のメモリは1つに畳む。
#[tokio::test]
async fn two_devices_with_their_own_data_meet() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    say(&b, Chat::General, "Bの総合チャット");
    memories::add(&b.conn(), &["朝型".to_string(), "犬が好き".to_string()]).unwrap();
    let b_task = tasks::create_task(&b.conn()).unwrap().id;
    b.conn()
        .execute("UPDATE tasks SET title = '散歩' WHERE id = ?1", [b_task])
        .unwrap();

    let choose = Choose::new(ChosenSide::Ours);
    let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
    assert_eq!(ra.unwrap().conflicts, 1, "{:?}", choose.seen());
    rb.unwrap();
    assert_eq!(choose.seen()[0].kind, crate::ConflictKind::General);
    assert_same(&a, &b);
    assert!(!alive_texts(&a, Chat::General).contains(&"Bの総合チャット".to_string()));
    let alive_memories: Vec<String> = memories::list(&a.conn())
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(
        alive_memories.iter().filter(|c| *c == "朝型").count(),
        1,
        "{alive_memories:?}"
    );
    assert!(alive_memories.contains(&"犬が好き".to_string()));
    a.task_uid("散歩");
}

#[tokio::test]
async fn a_memory_changed_on_both_sides_is_chosen_by_the_user() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let set = |d: &Device, content: &str| {
        d.conn()
            .execute(
                "UPDATE memories SET content = ?1 WHERE content = '朝型'",
                [content],
            )
            .unwrap();
    };
    set(&a, "早起き");
    set(&b, "夜更かし");
    let choose = Choose::new(ChosenSide::Theirs);
    let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
    assert_eq!(ra.unwrap().conflicts, 1);
    rb.unwrap();
    assert_eq!(choose.seen()[0].kind, crate::ConflictKind::Memory);
    assert_same(&a, &b);
    assert!(memories::list(&a.conn())
        .unwrap()
        .iter()
        .any(|m| m.content == "夜更かし"));
}

/// Aが確定したあと、Bが確定できなかった(Bのデータが途中で変わった)。やり直すと、同じ競合を
/// 聞き直さずにAの結果へ揃う。
#[tokio::test]
async fn a_sync_cut_after_the_listener_wrote_settles_when_retried() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    a.conn()
        .execute("UPDATE tasks SET title = '晩ご飯' WHERE uid = ?1", [&task])
        .unwrap();
    b.conn()
        .execute("UPDATE tasks SET title = '夕食' WHERE uid = ?1", [&task])
        .unwrap();

    let choose = Choose::new(ChosenSide::Ours);
    let b_db = b.ctx.db.clone();
    let (ra, rb) = sync_with(&a, &b, &choose, move |stage| {
        if stage == Stage::Finishing {
            memories::add(&b_db.lock().unwrap(), &["途中で足した".to_string()]).unwrap();
        }
    })
    .await;
    ra.unwrap_err();
    assert!(matches!(rb, Err(SyncError::Changed)), "{rb:?}");
    assert_eq!(b.task_uid("夕食"), task);

    let (ra, _) = sync(&a, &b).await;
    assert_eq!(ra.conflicts, 0);
    assert_same(&a, &b);
    assert_eq!(b.task_uid("晩ご飯"), task);
}

#[tokio::test]
async fn changes_travel_through_a_third_device() {
    let (a, b, c) = (Device::new(), Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    sync(&c, &b).await;
    let task = a.task_uid("夕飯");
    a.conn()
        .execute(
            "UPDATE tasks SET deadline = '2026-11-01' WHERE uid = ?1",
            [&task],
        )
        .unwrap();
    sync(&b, &a).await;
    sync(&b, &c).await;
    assert_same(&a, &c);
    assert_same(&b, &c);
}

/// 同じターンを2台で作り直したとき、取り込んだ試行は空いた番号に振り直す。返信の中身は同じ
/// 試行の記録を指したまま。
#[tokio::test]
async fn attempts_are_renumbered_on_the_receiving_side() {
    let (a, b) = (Device::new(), Device::new());
    let turn = "01JTURNR000000000000000000";
    say(&a, Chat::General, "質問");
    reply(&a, Chat::General, turn, 1, "最初の返事");
    sync(&a, &b).await;
    // Aでは、2回目の試行が返信を書かずに終わった(記録だけ)。
    let read = json!({ "tool": "get_memories", "arguments": {}, "result": [] });
    insert(
        &a.conn(),
        Chat::General,
        Role::Tool,
        Kind::ToolExecution,
        Origin::Turn {
            turn_id: turn,
            attempt_no: 2,
        },
        &read.to_string(),
        None,
    );
    // Bでは、作り直して2回目の試行が返信を書いた。
    {
        let conn = b.conn();
        let first: i64 = conn
            .query_row(
                "SELECT id FROM messages WHERE role = 'assistant'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        messages::soft_delete_normal_from(&conn, Chat::General, first).unwrap();
    }
    reply(&b, Chat::General, turn, 2, "作り直した返事");

    sync(&a, &b).await;
    assert_same(&a, &b);
    assert_eq!(alive_texts(&a, Chat::General), ["質問", "作り直した返事"]);
    let attempts: Vec<i64> = a
        .conn()
        .prepare("SELECT DISTINCT attempt_no FROM messages WHERE turn_id = ?1 ORDER BY 1")
        .unwrap()
        .query_map([turn], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(attempts, [1, 2, 3]);
}

/// 選んだ側と相手で日時が違っても(相手の方が遅く変えた・早く済ませた)、どちらを選んでも両側が
/// 同じ結果に確定する(日時は、自動の結合と同じく合わせる)。
#[tokio::test]
async fn resolving_with_different_times_on_each_side_settles_both_sides() {
    for side in [ChosenSide::Ours, ChosenSide::Theirs] {
        let (a, b) = (Device::new(), Device::new());
        populate(&a).await;
        sync(&a, &b).await;
        let task = a.task_uid("夕飯");
        let set = |d: &Device, title: &str, at: &str, done: &str| {
            let conn = d.conn();
            conn.execute(
                "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE uid = ?3",
                [title, at, task.as_str()],
            )
            .unwrap();
            conn.execute(
                "UPDATE task_steps SET done_at = ?1 WHERE description = '買い出し'",
                [done],
            )
            .unwrap();
        };
        set(&a, "晩ご飯", "2026-10-02T00:00:00Z", "2026-10-02T00:00:00Z");
        set(&b, "夕食", "2026-10-03T00:00:00Z", "2026-10-01T00:00:00Z");
        let choose = Choose::new(side);
        let (ra, rb) = sync_with(&a, &b, &choose, |_| {}).await;
        assert_eq!(ra.unwrap().conflicts, 1);
        rb.unwrap();
        assert_same(&a, &b);
        let row = &a
            .transport()
            .tasks
            .into_iter()
            .find(|t| t.uid == task)
            .unwrap();
        assert_eq!(row.updated_at, "2026-10-03T00:00:00Z");
    }
}

#[tokio::test]
async fn the_same_conflict_settled_both_ways_is_asked_again() {
    let (a, b, c, d) = (Device::new(), Device::new(), Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    sync(&c, &a).await;
    sync(&d, &a).await;
    let task = a.task_uid("夕飯");
    let rename = |dev: &Device, title: &str| {
        dev.conn()
            .execute(
                "UPDATE tasks SET title = ?1 WHERE uid = ?2",
                [title, task.as_str()],
            )
            .unwrap();
    };
    rename(&a, "晩ご飯");
    rename(&b, "夕食");
    // cはbの変更を、dはaの変更を持って、それぞれ相手と逆の側を選ぶ。
    sync(&c, &b).await;
    sync(&d, &a).await;
    let ours = Choose::new(ChosenSide::Ours);
    let (r, _) = sync_with(&a, &c, &ours, |_| {}).await;
    assert_eq!(r.unwrap().conflicts, 1);
    let (r, _) = sync_with(&b, &d, &ours, |_| {}).await;
    assert_eq!(r.unwrap().conflicts, 1);

    let again = Choose::new(ChosenSide::Ours);
    let (r, rb) = sync_with(&a, &b, &again, |_| {}).await;
    assert_eq!(r.unwrap().conflicts, 1);
    rb.unwrap();
    assert_same(&a, &b);
}

/// 同じ本文のメモリを2組が別々に畳んでも、消す行が同じなので揃う(「版と結合」)。
#[tokio::test]
async fn memories_folded_on_two_pairs_agree() {
    let (a, b, c, d) = (Device::new(), Device::new(), Device::new(), Device::new());
    memories::add(&a.conn(), &["朝型".to_string()]).unwrap();
    memories::add(&b.conn(), &["朝型".to_string()]).unwrap();
    sync(&c, &a).await;
    sync(&d, &b).await;
    sync(&c, &b).await;
    sync(&d, &a).await;
    let (rc, rd) = sync(&c, &d).await;
    assert_eq!(rc.conflicts, 0);
    let _ = rd;
    assert_same(&c, &d);
    assert_eq!(memories::list(&c.conn()).unwrap().len(), 1);
}

/// 書き終えた合図の端末IDは、名乗った端末IDか、巻き戻りで作り直した新しい端末ID。
#[test]
fn the_final_device_id_is_the_one_introduced() {
    let introduced = "a".repeat(32);
    let agreed = |peer_renews| Agreed {
        send_from: 0,
        peer: introduced.clone(),
        peer_renews,
    };
    agreed(false).check_final_device_id(&introduced).unwrap();
    for other in ["b".repeat(32), "x".to_string()] {
        assert!(
            agreed(false).check_final_device_id(&other).is_err(),
            "{other}"
        );
    }
    agreed(true).check_final_device_id(&"b".repeat(32)).unwrap();
    for other in [introduced.clone(), "x".to_string()] {
        assert!(
            agreed(true).check_final_device_id(&other).is_err(),
            "{other}"
        );
    }
}

#[test]
fn a_choice_is_kept_only_while_the_conflict_stays_the_same() {
    let device = Device::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(populate(&device));
    let (rows, _) =
        db_sync::transaction(&device.conn(), |tx| convert::read(tx, Filter::All)).unwrap();
    let conflict = Conflict {
        unit: merge::Unit::Task(rows.tasks[0].uid.clone()),
        ours: rows.clone(),
        theirs: rows,
    };
    let chosen = Chosen::new(Pick::Theirs, &conflict);
    assert!(chosen.is_for(&conflict));

    let mut changed = conflict.clone();
    changed.ours.steps[0].description = "別のプロセスが書いた".to_string();
    assert!(!chosen.is_for(&changed));
}

/// 競合を見せられたら、1回目だけ`write`で待ち受ける側に書き込んでから(別のプロセスの書き込み)、
/// 相手の側を選ぶ。
struct WriteWhileChoosing<'a> {
    write: Box<dyn Fn() + Sync + 'a>,
    calls: Mutex<usize>,
}

impl Resolver for WriteWhileChoosing<'_> {
    fn choose(&self, conflicts: Vec<ConflictView>) -> BoxFuture<'_, Option<Vec<ConflictChoice>>> {
        let mut calls = self.calls.lock().unwrap();
        if *calls == 0 {
            (self.write)();
        }
        *calls += 1;
        let choices = conflicts
            .iter()
            .map(|c| ConflictChoice {
                id: c.id.clone(),
                side: ChosenSide::Theirs,
            })
            .collect();
        Box::pin(async move { Some(choices) })
    }
}

/// 選んでいる間に別のプロセスが書くと、確定をやり直す。そのとき、選んだ競合の中身が変わっていれば
/// 選び直させ、変わっていなければ聞き直さない。選び直したあとは、書き込みの段を知らせ直す(画面は
/// 選んでいる間だけ止められる)。
#[tokio::test]
async fn a_conflict_that_changed_after_it_was_chosen_is_asked_again() {
    for (touches_the_conflict, asked) in [(true, 2), (false, 1)] {
        let (a, b) = (Device::new(), Device::new());
        populate(&a).await;
        sync(&a, &b).await;
        let task = a.task_uid("夕飯");
        let (a_task, b_task) = (a.task_id(&task), b.task_id(&task));
        a.conn()
            .execute("UPDATE tasks SET title = '晩ご飯' WHERE id = ?1", [a_task])
            .unwrap();
        b.conn()
            .execute("UPDATE tasks SET title = '夕食' WHERE id = ?1", [b_task])
            .unwrap();

        let resolver = WriteWhileChoosing {
            write: Box::new(|| {
                if touches_the_conflict {
                    a.conn()
                        .execute(
                            "UPDATE tasks SET description = '別のプロセス' WHERE id = ?1",
                            [a_task],
                        )
                        .unwrap();
                } else {
                    memories::add(&a.conn(), &["別のプロセス".to_string()]).unwrap();
                }
            }),
            calls: Mutex::new(0),
        };
        let stages = Mutex::new(Vec::new());
        let (ra, rb) = sync_with_stages(
            &a,
            &b,
            &resolver,
            |stage| stages.lock().unwrap().push(stage),
            |_| {},
        )
        .await;
        assert_eq!(ra.unwrap().conflicts, 1);
        rb.unwrap();
        assert_eq!(
            *resolver.calls.lock().unwrap(),
            asked,
            "touches_the_conflict: {touches_the_conflict}"
        );
        assert_eq!(
            *stages.lock().unwrap(),
            vec![Stage::Finishing; asked],
            "touches_the_conflict: {touches_the_conflict}"
        );
        assert_same(&a, &b);
    }
}

/// 書き込む前の確認に出す計画を、計画どおりに書く場合の結果と比べる。
#[tokio::test]
async fn the_confirm_plan_tells_whose_writes() {
    // 相手にしか無いデータを受け取る側。この端末に書くことになるすべての単位を数える。
    let (a, b) = (Device::new(), Device::new());
    populate(&b).await;
    let confirming = Confirming {
        first: WriteAnswer::Write,
        seen: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &NoConflicts, &confirming, |_| {}, |_| {}).await;
    ra.unwrap();
    rb.unwrap();
    assert_eq!(
        *confirming.seen.lock().unwrap(),
        vec![WritePlan {
            ours: WriteCounts {
                tasks: 2,
                conversations: 2,
                memories: 2,
            },
            theirs: WriteCounts::default(),
        }]
    );

    // 送るだけの側。相手に書き込むことになるものだけが変わる。
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    let confirming = Confirming {
        first: WriteAnswer::Write,
        seen: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &NoConflicts, &confirming, |_| {}, |_| {}).await;
    ra.unwrap();
    assert_eq!(
        *confirming.seen.lock().unwrap(),
        vec![WritePlan {
            ours: WriteCounts::default(),
            theirs: WriteCounts {
                tasks: 2,
                conversations: 2,
                memories: 2,
            },
        }]
    );
    // 相手は、この端末にあったすべての行を書き込む(4節7)。
    assert_eq!(rb.unwrap().changed.tasks, 2);
}

/// 確認の計画には、同じ本文のメモリを畳む書き込みも含まれる(8節)。
#[tokio::test]
async fn the_confirm_plan_counts_the_folded_memories() {
    let (a, b) = (Device::new(), Device::new());
    populate(&b).await;
    memories::add(&a.conn(), &["朝型".to_string()]).unwrap();
    let confirming = Confirming {
        first: WriteAnswer::Write,
        seen: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &NoConflicts, &confirming, |_| {}, |_| {}).await;
    ra.unwrap();
    rb.unwrap();
    // この端末に書くのは、相手のメモリ2行と、同じ本文を畳む行1つ。送り返す行の数は、1の大きい方
    // がAの行とBの行のどちらか(乱数)に応じて1または2になる。
    let plans = confirming.seen.lock().unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].ours.tasks, 2);
    assert_eq!(plans[0].ours.conversations, 2);
    assert_eq!(plans[0].ours.memories, 3);
    assert!(plans[0].theirs.memories == 1 || plans[0].theirs.memories == 2);
    assert_eq!(plans[0].theirs.tasks, 0);
    assert_eq!(plans[0].theirs.conversations, 0);
}

/// 競合が無い確認での「戻る」は、書き込まずに終わる(戻る先が無い)。
#[tokio::test]
async fn back_from_the_confirm_without_conflicts_cancels() {
    let (a, b) = (Device::new(), Device::new());
    populate(&b).await;
    let confirming = Confirming {
        first: WriteAnswer::Back,
        seen: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &NoConflicts, &confirming, |_| {}, |_| {}).await;
    assert!(matches!(ra, Err(SyncError::Cancelled)), "{ra:?}");
    assert!(rb.is_err(), "{rb:?}");
    assert_eq!(a.transport().tasks, []);
    assert_eq!(b.transport().tasks.len(), 2);
}

/// 競合がある確認での「戻る」は、競合の選択に戻る。選び直したときは、その計画でもう一度出す。
#[tokio::test]
async fn back_from_the_confirm_returns_to_the_conflicts() {
    let (a, b) = (Device::new(), Device::new());
    populate(&a).await;
    sync(&a, &b).await;
    let task = a.task_uid("夕飯");
    let (a_task, b_task) = (a.task_id(&task), b.task_id(&task));
    a.conn()
        .execute("UPDATE tasks SET title = '晩ご飯' WHERE id = ?1", [a_task])
        .unwrap();
    b.conn()
        .execute("UPDATE tasks SET title = '夕食' WHERE id = ?1", [b_task])
        .unwrap();
    let choose = Choose::new(ChosenSide::Ours);
    let confirming = Confirming {
        first: WriteAnswer::Back,
        seen: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &choose, &confirming, |_| {}, |_| {}).await;
    assert_eq!(ra.unwrap().conflicts, 1);
    rb.unwrap();
    // 競合は2回見せられ、出された計画は2つ(選び直した結果の値でもう一度)。
    assert_eq!(choose.seen().len(), 2);
    let plans = confirming.seen.lock().unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[1].ours.tasks, 1);
    assert_eq!(plans[1].ours.conversations, 1);
    assert_eq!(plans[1].ours.memories, 0);
    assert_eq!(plans[1].theirs.tasks, 1);
    assert_same(&a, &b);
}

/// 確認を見せている間に別のプロセスが書くと、確定をやり直す。その計画でもう一度確認に出す。
struct WriteWhileConfirming<'a> {
    write: Box<dyn Fn() + Sync + 'a>,
    calls: Mutex<usize>,
    plans: Mutex<Vec<WritePlan>>,
}

impl Confirmer for WriteWhileConfirming<'_> {
    fn confirm(&self, plan: WritePlan) -> BoxFuture<'_, Option<WriteAnswer>> {
        let mut calls = self.calls.lock().unwrap();
        if *calls == 0 {
            (self.write)();
        }
        *calls += 1;
        self.plans.lock().unwrap().push(plan);
        Box::pin(async { Some(WriteAnswer::Write) })
    }
}

#[tokio::test]
async fn a_write_during_the_confirm_asks_again() {
    let (a, b) = (Device::new(), Device::new());
    populate(&b).await;
    let confirmer = WriteWhileConfirming {
        write: Box::new(|| {
            memories::add(&a.conn(), &["別のプロセス".to_string()]).unwrap();
        }),
        calls: Mutex::new(0),
        plans: Mutex::new(Vec::new()),
    };
    let (ra, rb) = sync_with_confirmed(&a, &b, &NoConflicts, &confirmer, |_| {}, |_| {}).await;
    ra.unwrap();
    rb.unwrap();
    assert_eq!(*confirmer.calls.lock().unwrap(), 2);
    // 確認のあとに書かれた行は、この端末では書かず、相手へ送る。
    let plans = confirmer.plans.lock().unwrap();
    assert_eq!(plans[0].theirs.memories, 0);
    assert_eq!(plans[1].theirs.memories, 1);
    assert_same(&a, &b);
}
