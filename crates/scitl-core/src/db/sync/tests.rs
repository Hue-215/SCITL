use super::*;
use crate::db;

fn versions(conn: &Connection, table: &str) -> Vec<(String, i64)> {
    select(
        conn,
        &format!("SELECT sync_version, sync_changed FROM {table} ORDER BY id"),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn marked_rows_get_this_devices_number_and_lose_the_mark() {
    let conn = db::open_in_memory().unwrap();
    let task_id = db::tasks::create_task(&conn).unwrap().id;
    db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();
    assert_eq!(versions(&conn, "tasks"), [("{}".to_string(), 1)]);

    let rows = transaction(&conn, |tx| {
        tx.stamp()?;
        tx.rows(Filter::All)
    })
    .unwrap();
    let device = device_state(&conn).unwrap().device_id;
    for table in VERSIONED.iter().filter(|t| **t != "messages") {
        let rows = versions(&conn, table);
        assert_eq!(rows.len(), 1, "{table}");
        let (version, changed) = &rows[0];
        let version: serde_json::Value = serde_json::from_str(version).unwrap();
        assert!(version[&device].as_i64().unwrap() > 1, "{table}");
        assert_eq!(*changed, 0, "{table}");
    }
    assert_eq!(rows.tasks[0].version, versions(&conn, "tasks")[0].0);

    // 印の無い行の版は変えない。
    let before = versions(&conn, "tasks");
    transaction(&conn, |tx| tx.stamp()).unwrap();
    assert_eq!(versions(&conn, "tasks"), before);
}

#[test]
fn rows_can_be_read_by_change_by_uid_or_by_unit() {
    let conn = db::open_in_memory().unwrap();
    let first = db::tasks::create_task(&conn).unwrap().id;
    db::task_steps::add_steps(&conn, first, &["買い出し".to_string()]).unwrap();
    let since = device_state(&conn).unwrap().counter;
    let second = db::tasks::create_task(&conn).unwrap().id;
    db::tasks::delete_task(&conn, second).unwrap();
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();

    let changed = transaction(&conn, |tx| tx.rows(Filter::Since(since))).unwrap();
    assert_eq!(changed.tasks.len(), 1);
    assert_eq!(changed.tasks[0].id, second);
    assert!(changed.tasks[0].deleted_at.is_some());
    assert!(changed.steps.is_empty());
    assert_eq!(changed.memories.len(), 1);

    let all = transaction(&conn, |tx| tx.rows(Filter::All)).unwrap();
    let uid = all.tasks[0].uid.clone();
    let by_uid = transaction(&conn, |tx| {
        tx.rows(Filter::Uids(std::slice::from_ref(&uid)))
    })
    .unwrap();
    assert_eq!(by_uid.tasks, all.tasks[..1]);
    assert!(by_uid.steps.is_empty());

    let unit = transaction(&conn, |tx| {
        tx.rows(Filter::Units {
            tasks: std::slice::from_ref(&uid),
            general: false,
        })
    })
    .unwrap();
    assert_eq!(unit.tasks, all.tasks[..1]);
    assert_eq!(unit.steps, all.steps);
    assert!(unit.memories.is_empty());
}

#[test]
fn the_attempt_key_is_the_first_row_of_the_attempt() {
    use db::messages::{insert_message, Chat, Kind, NewMessage, Origin, Role};
    let conn = db::open_in_memory().unwrap();
    let write = |attempt_no| {
        insert_message(
            &conn,
            NewMessage {
                chat: Chat::General,
                role: Role::Tool,
                content: r#"{"tool":"get_memories","arguments":{},"result":{"memories":[]}}"#,
                kind: Kind::ToolExecution,
                origin: Origin::Turn {
                    turn_id: "t1",
                    attempt_no,
                },
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap()
    };
    write(1);
    write(1);
    write(2);
    let rows = transaction(&conn, |tx| tx.rows(Filter::All))
        .unwrap()
        .messages;
    assert_eq!(rows[1].attempt_key.as_ref(), Some(&rows[0].uid));
    assert_eq!(rows[2].attempt_key.as_ref(), Some(&rows[2].uid));
}

#[test]
fn the_highest_number_seen_of_a_device_comes_from_the_versions() {
    let conn = db::open_in_memory().unwrap();
    let peer = "a".repeat(32);
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();
    conn.execute(
        "UPDATE memories SET sync_version = json_object(?1, 7)",
        [&peer],
    )
    .unwrap();
    transaction(&conn, |tx| {
        assert_eq!(tx.seen_of(&peer)?, 7);
        assert_eq!(tx.seen_of(&"b".repeat(32))?, 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn the_token_can_be_replaced_with_or_without_the_device_id() {
    let conn = db::open_in_memory().unwrap();
    let first = device_token(&conn).unwrap();
    let (before, rotated) = replace_device_token(&conn, false).unwrap();
    assert_eq!(before, first);
    assert_eq!(rotated.device_id, before.device_id);
    assert_ne!(rotated.token, before.token);
    let (before, renewed) = replace_device_token(&conn, true).unwrap();
    assert_eq!(before, rotated);
    assert_ne!(renewed.device_id, rotated.device_id);
    assert_ne!(renewed.token, rotated.token);
    assert_eq!(device_token(&conn).unwrap(), renewed);
    assert_eq!(device_state(&conn).unwrap().device_id, renewed.device_id);
}

#[test]
fn restoring_the_token_keeps_a_later_replacement() {
    let conn = db::open_in_memory().unwrap();
    let (before, replaced) = replace_device_token(&conn, true).unwrap();
    restore_device_token(&conn, &before, &replaced).unwrap();
    assert_eq!(device_token(&conn).unwrap(), before);

    // 別プロセスが替えたあとなら戻さない。
    let (before, replaced) = replace_device_token(&conn, true).unwrap();
    let (_, later) = replace_device_token(&conn, false).unwrap();
    restore_device_token(&conn, &before, &replaced).unwrap();
    assert_eq!(device_token(&conn).unwrap(), later);
}

#[test]
fn a_malformed_token_is_refused() {
    let conn = db::open_in_memory().unwrap();
    for token in [
        "",
        "ABCDEF0123456789ABCDEF0123456789",
        "0123",
        &"g".repeat(32),
    ] {
        assert!(
            conn.execute("UPDATE sync_state SET device_token = ?1", [token])
                .is_err(),
            "{token}"
        );
    }
    assert!(conn
        .execute("UPDATE sync_state SET device_token = 12345", [])
        .is_err());
}

#[test]
fn written_rows_keep_their_uid_and_version_and_lose_the_mark() {
    let source = db::open_in_memory().unwrap();
    db::memories::add(&source, &["朝型".to_string()]).unwrap();
    let rows = transaction(&source, |tx| {
        tx.stamp()?;
        tx.rows(Filter::All)
    })
    .unwrap();

    let target = db::open_in_memory().unwrap();
    let peer = "a".repeat(32);
    let cursor = Cursor {
        sent_seq: 3,
        received_seq: 42,
    };
    transaction(&target, |tx| {
        tx.insert_memory(&rows.memories[0])?;
        tx.clear_changed()?;
        tx.record_cursor(&peer, cursor)
    })
    .unwrap();

    let copied = transaction(&target, |tx| tx.rows(Filter::All)).unwrap();
    assert_eq!(copied.memories[0].uid, rows.memories[0].uid);
    assert_eq!(copied.memories[0].version, rows.memories[0].version);
    assert_eq!(versions(&target, "memories")[0].1, 0);
    assert_eq!(super::cursor(&target, &peer).unwrap(), cursor);
    assert_eq!(
        super::cursor(&target, &"b".repeat(32)).unwrap(),
        Cursor::default()
    );
}

#[test]
fn updates_change_only_the_columns_that_change_after_writing() {
    let conn = db::open_in_memory().unwrap();
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();
    let mut row = transaction(&conn, |tx| tx.rows(Filter::All))
        .unwrap()
        .memories[0]
        .clone();
    row.content = "夜型".to_string();
    row.created_at = "2000-01-01T00:00:00Z".to_string();
    row.version = r#"{"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":3}"#.to_string();
    transaction(&conn, |tx| tx.update_memory(&row)).unwrap();
    let written = transaction(&conn, |tx| tx.rows(Filter::All))
        .unwrap()
        .memories[0]
        .clone();
    assert_eq!(written.content, "夜型");
    assert_eq!(written.version, row.version);
    assert_ne!(written.created_at, row.created_at);
}

#[test]
fn a_failed_transaction_leaves_nothing() {
    let source = db::open_in_memory().unwrap();
    db::memories::add(&source, &["朝型".to_string()]).unwrap();
    let rows = transaction(&source, |tx| tx.rows(Filter::All)).unwrap();

    let target = db::open_in_memory().unwrap();
    let result: Result<()> = transaction(&target, |tx| {
        tx.insert_memory(&rows.memories[0])?;
        Err(crate::CoreError::Internal("stop".to_string()))
    });
    assert!(result.is_err());
    assert!(transaction(&target, |tx| tx.rows(Filter::All))
        .unwrap()
        .memories
        .is_empty());
}

#[test]
fn rules_accept_what_the_app_stores_and_refuse_the_rest() {
    rules::title("買い物").unwrap();
    assert!(rules::title(" 買い物").is_err());
    assert!(rules::title("「買い物」").is_err());
    assert!(rules::title(&"あ".repeat(41)).is_err());
    rules::task_description("牛乳と卵\n2個ずつ").unwrap();
    assert!(rules::task_description(" ").is_err());
    rules::deadline("2026-02-28").unwrap();
    assert!(rules::deadline("2026-02-30").is_err());
    rules::step_description("買い出し").unwrap();
    assert!(rules::step_description("買い出し ").is_err());
    rules::memory_content("朝型").unwrap();
    assert!(rules::memory_content("朝\n型").is_err());
    rules::user_message_content("こんにちは").unwrap();
    rules::user_message_content("").unwrap();
    assert!(rules::user_message_content("こんにちは\n").is_err());
    rules::timestamp(&db::now_iso8601()).unwrap();
    for bad in [
        "2026-10-14T12:00:00",
        "2026-10-14 12:00:00Z",
        "2026-13-14T12:00:00Z",
        "2026-10-14T12:00:00.5Z",
    ] {
        assert!(rules::timestamp(bad).is_err(), "{bad}");
    }
}
