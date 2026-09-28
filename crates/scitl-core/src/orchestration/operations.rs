//! 応答生成以外の経路(画面。将来はCLI・MCP)からのタスク操作(Issue #75)。変更と会話ログへの
//! 記録を同じトランザクションで書く(data-model.md「応答生成以外の経路での操作の記録」)。
//! 状態の列は現在の状態だけを持ち、いつ何をしたかはこの記録が持つ。
//!
//! どの操作も、そのタスクが応答を生成中なら断る([`super::turn`]の生成中の集合)。ターンの
//! 途中に`turn_id`の無い記録が挟まるとターンの表示が割れ、モデルの`update_task`と同じタスクへ
//! 並んで書くことにもなる。
//!
//! 状態が変わらない操作(整形すると今と同じになるタイトル、アーカイブ済みのアーカイブ)は、
//! 変更も記録もしない。何も起きなかった記録を会話ログに残さないため。変わるかどうかの判定は
//! ここに置き、画面には整形の規則を写さない。

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::messages::{self, Chat, Kind, NewMessage, OperationSource, Origin, Role};
use crate::db::tasks::{self, Task};
use crate::db::{in_transaction, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::in_flight::InFlightSet;
use crate::orchestration::tool_record::ToolExecutionRecord;
use crate::orchestration::turn::begin_generating;
use crate::tools::update_task;

/// 削除の記録に載せる操作名。モデルには公開しないツール(tools.md 2節)だが、記録の語彙は
/// 他の操作と同じくツール名に揃える。
const DELETE_TASK: &str = "delete_task";

/// タイトルの変更。整形すると空になるタイトルは断る(モデルの`update_task`は空を「指定なし」
/// として扱うが、画面から空を送るのは取り消しと同じなので、変更の記録を残さない)。
pub async fn rename_task(
    db: SharedConnection,
    generating: &InFlightSet<Chat>,
    source: OperationSource,
    task_id: i64,
    title: String,
) -> Result<()> {
    let sanitized = tasks::sanitize_title(&title);
    if sanitized.is_empty() {
        return Err(CoreError::InvalidArgument {
            name: "title".to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    let unchanged = move |task: &Task| task.title.as_deref() == Some(sanitized.as_str());
    update(
        db,
        generating,
        source,
        task_id,
        json!({ "title": title }),
        unchanged,
    )
    .await
}

/// アーカイブ・アーカイブ解除。
pub async fn set_task_archived(
    db: SharedConnection,
    generating: &InFlightSet<Chat>,
    source: OperationSource,
    task_id: i64,
    archived: bool,
) -> Result<()> {
    let status = if archived { "archived" } else { "unarchived" };
    let unchanged = move |task: &Task| task.archived_at.is_some() == archived;
    update(
        db,
        generating,
        source,
        task_id,
        json!({ "status": status }),
        unchanged,
    )
    .await
}

/// タスクの論理削除。記録は削除したタスク自身の会話に置く(削除を取り消せば一緒に戻る)。
pub async fn delete_task(
    db: SharedConnection,
    generating: &InFlightSet<Chat>,
    source: OperationSource,
    task_id: i64,
) -> Result<()> {
    let _generating = begin_generating(generating, Chat::Task(task_id))?;
    with_conn(db, move |conn| {
        in_transaction(conn, |conn| {
            let deleted = tasks::delete_task(conn, task_id)?;
            let result = serde_json::to_value(deleted).expect("Task serialization cannot fail");
            record(conn, source, task_id, DELETE_TASK, json!({}), result)
        })
    })
    .await
}

/// タスクチャット版の`update_task`ツールと同じ検証・実行を通す。記録する引数は、実行した
/// 引数そのもの(記録用と実行用を別に組み立てると食い違う余地が残る)。`unchanged`が今の
/// タスクについて真なら、変更も記録もしない。
async fn update(
    db: SharedConnection,
    generating: &InFlightSet<Chat>,
    source: OperationSource,
    task_id: i64,
    arguments: Value,
    unchanged: impl FnOnce(&Task) -> bool + Send + 'static,
) -> Result<()> {
    let _generating = begin_generating(generating, Chat::Task(task_id))?;
    with_conn(db, move |conn| {
        in_transaction(conn, |conn| {
            if unchanged(&tasks::get_task(conn, task_id)?) {
                return Ok(());
            }
            let result = update_task::execute(conn, task_id, &arguments)?;
            record(conn, source, task_id, update_task::NAME, arguments, result)
        })
    })
    .await
}

fn record(
    conn: &Connection,
    source: OperationSource,
    task_id: i64,
    tool: &str,
    arguments: Value,
    result: Value,
) -> Result<()> {
    let content = serde_json::to_string(&ToolExecutionRecord {
        tool: tool.to_string(),
        arguments,
        result,
        tool_kind: None,
        call_id: None,
    })
    .expect("a record of JSON values serializes");
    messages::insert_message(
        conn,
        NewMessage {
            task_id: Some(task_id),
            role: Role::Tool,
            content: &content,
            kind: Kind::ToolExecution,
            origin: Origin::Operation(source),
            error_kind: None,
            error_detail: None,
            reasoning: None,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::db;

    struct Fixture {
        db: SharedConnection,
        generating: InFlightSet<Chat>,
        task_id: i64,
    }

    impl Fixture {
        fn new() -> Self {
            let conn = db::open_in_memory().unwrap();
            let task_id = tasks::create_task(&conn).unwrap().id;
            Self {
                db: Arc::new(Mutex::new(conn)),
                generating: InFlightSet::new(),
                task_id,
            }
        }

        fn records(&self) -> Vec<(Option<String>, Value)> {
            let conn = self.db.lock().unwrap();
            messages::list_for_chat(&conn, Chat::Task(self.task_id))
                .unwrap()
                .into_iter()
                .map(|m| (m.source, serde_json::from_str(&m.content).unwrap()))
                .collect()
        }

        fn task(&self) -> Task {
            tasks::get_task(&self.db.lock().unwrap(), self.task_id).unwrap()
        }
    }

    #[tokio::test]
    async fn rename_records_the_executed_arguments_and_the_result() {
        let f = Fixture::new();
        rename_task(
            f.db.clone(),
            &f.generating,
            OperationSource::Ui,
            f.task_id,
            "「買い物」".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(f.task().title.as_deref(), Some("買い物"));
        let records = f.records();
        assert_eq!(records.len(), 1);
        let (source, content) = &records[0];
        assert_eq!(source.as_deref(), Some("ui"));
        assert_eq!(
            content,
            &json!({
                "tool": "update_task",
                "arguments": { "title": "「買い物」" },
                "result": serde_json::to_value(f.task()).unwrap(),
            })
        );
    }

    #[tokio::test]
    async fn a_blank_title_changes_and_records_nothing() {
        let f = Fixture::new();
        for title in ["", "  ", "「」"] {
            let err = rename_task(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                title.to_string(),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, CoreError::InvalidArgument { .. }));
        }
        assert!(f.records().is_empty());
    }

    #[tokio::test]
    async fn archive_and_unarchive_are_recorded_as_update_task() {
        let f = Fixture::new();
        for archived in [true, false] {
            set_task_archived(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                archived,
            )
            .await
            .unwrap();
            assert_eq!(f.task().archived_at.is_some(), archived);
        }
        let arguments: Vec<_> = f
            .records()
            .into_iter()
            .map(|(_, c)| c["arguments"].clone())
            .collect();
        assert_eq!(
            arguments,
            vec![
                json!({ "status": "archived" }),
                json!({ "status": "unarchived" })
            ]
        );
    }

    #[tokio::test]
    async fn operations_that_change_nothing_are_not_recorded() {
        let f = Fixture::new();
        let rename = |title: &str| {
            rename_task(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                title.to_string(),
            )
        };
        let archive = |archived| {
            set_task_archived(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                archived,
            )
        };

        // まだアーカイブしていないタスクのアーカイブ解除。
        archive(false).await.unwrap();
        assert!(f.records().is_empty());

        rename("買い物").await.unwrap();
        archive(true).await.unwrap();
        let archived = f.task();
        assert_eq!(f.records().len(), 2);

        // 整形すると今と同じになるタイトルと、アーカイブ済みのアーカイブ。
        rename("「買い物」").await.unwrap();
        rename(" 買い物 ").await.unwrap();
        archive(true).await.unwrap();
        assert_eq!(f.records().len(), 2);
        assert_eq!(f.task().updated_at, archived.updated_at);
    }

    #[tokio::test]
    async fn delete_keeps_its_record_in_the_deleted_tasks_conversation() {
        let f = Fixture::new();
        delete_task(f.db.clone(), &f.generating, OperationSource::Ui, f.task_id)
            .await
            .unwrap();

        let conn = f.db.lock().unwrap();
        assert!(matches!(
            tasks::get_task(&conn, f.task_id),
            Err(CoreError::TaskNotFound(_))
        ));
        let rows = messages::list_for_chat(&conn, Chat::Task(f.task_id)).unwrap();
        assert_eq!(rows.len(), 1);
        let content: Value = serde_json::from_str(&rows[0].content).unwrap();
        assert_eq!(content["tool"], "delete_task");
        assert_eq!(content["arguments"], json!({}));
        assert_eq!(content["result"]["id"], f.task_id);
        assert!(content["result"]["deleted_at"].is_string());
        assert!(messages::list_for_chat(&conn, Chat::General)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn operations_are_refused_while_the_task_is_generating() {
        let f = Fixture::new();
        let _in_progress = f.generating.try_begin(Chat::Task(f.task_id)).unwrap();

        let results = [
            rename_task(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                "新しい名前".to_string(),
            )
            .await,
            set_task_archived(
                f.db.clone(),
                &f.generating,
                OperationSource::Ui,
                f.task_id,
                true,
            )
            .await,
            delete_task(f.db.clone(), &f.generating, OperationSource::Ui, f.task_id).await,
        ];
        for result in results {
            assert!(matches!(result, Err(CoreError::ChatBusy(_))));
        }
        assert!(f.task().title.is_none());
        assert!(f.records().is_empty());
    }

    #[tokio::test]
    async fn a_failed_change_leaves_no_record() {
        let f = Fixture::new();
        delete_task(f.db.clone(), &f.generating, OperationSource::Ui, f.task_id)
            .await
            .unwrap();

        let err = rename_task(
            f.db.clone(),
            &f.generating,
            OperationSource::Ui,
            f.task_id,
            "名前".to_string(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, CoreError::TaskNotFound(_)));
        let conn = f.db.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "削除の記録だけが残る");
    }
}
