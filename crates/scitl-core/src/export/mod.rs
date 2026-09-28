//! Markdownエクスポート(Issue #72)。全タスクと総合チャットを、それぞれ1つのMarkdownファイルに
//! 書き出す(legacy/backend.md 10節)。書き出すのは画面から見られるものだけで、削除した
//! タスク・工程・発言と、会話から外れたターン(古い試行・破棄されたターン)は含めない。
//! 思考と`error_detail`も含めない(data-model.md messages)。
//!
//! 書き出し先は呼び出し側が決める。画面からはパスを受け取らない(architecture.md 13節)。

mod markdown;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;
use ulid::Ulid;

use crate::attachments::{safe_file_name, AttachmentStore, Attachments};
use crate::blocking;
use crate::db::attachments::{self, Attachment, AttachmentContent};
use crate::db::messages::{self, Chat, Message};
use crate::db::task_steps::{self, TaskStep};
use crate::db::tasks::{self, Task};
use crate::db::{now_iso8601, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use markdown::{AttachmentLink, Entry};

const ATTACHMENTS_DIR: &str = "attachments";
/// ファイル名に入れるタイトルの長さ。フォルダまでのパスと合わせて、Windowsのパスの長さの
/// 上限に届かないよう短めにする(タイトルの全体はファイルの中にある)。
const TITLE_CHARS_IN_FILE_NAME: usize = 40;

/// 書き出しの結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ExportSummary {
    /// 書き出したフォルダの名前(`root`の直下)。
    pub folder: String,
    pub tasks: usize,
    pub attachments: usize,
    /// 実体を読めない・書けないために同梱できなかった添付の数。
    pub missing_attachments: usize,
}

/// `root`の下に書き出した日時の名前のフォルダを作り、その中へ書き出す。前回までの書き出しには
/// 触れない。
///
/// DBは1回のロックの中で全部を読み、ファイルの書き込みにはロックを持ち込まない。
pub async fn export_markdown(
    db: SharedConnection,
    attachments: &Attachments,
    root: PathBuf,
) -> Result<ExportSummary> {
    let snapshot = with_conn(db, Snapshot::read).await?;
    let store = attachments.store();
    blocking::run(move || write(&snapshot, &store, &root)).await
}

/// 書き出し先のフォルダをOSで開く。まだ一度も書き出していなければ空のフォルダを作って開く。
pub fn open_folder(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(io_error("create the export folder"))?;
    open::that_detached(root).map_err(io_error("open the export folder"))
}

struct ChatRows {
    messages: Vec<Message>,
    attachments: HashMap<i64, Vec<Attachment>>,
}

impl ChatRows {
    /// 並びと絞り込みは画面の会話と同じ(`list_rows_for_chat`)。
    fn read(conn: &Connection, chat: Chat) -> Result<Self> {
        Ok(Self {
            messages: messages::list_rows_for_chat(conn, chat)?,
            attachments: attachments::for_chat(conn, chat)?,
        })
    }
}

struct Snapshot {
    tasks: Vec<(Task, Vec<TaskStep>, ChatRows)>,
    general: ChatRows,
}

impl Snapshot {
    fn read(conn: &Connection) -> Result<Self> {
        let tasks = tasks::list_all(conn)?
            .into_iter()
            .map(|task| {
                let steps = task_steps::list_for_task(conn, task.id)?;
                let chat = ChatRows::read(conn, Chat::Task(task.id))?;
                Ok((task, steps, chat))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            tasks,
            general: ChatRows::read(conn, Chat::General)?,
        })
    }
}

/// 隠しの一時フォルダに書いてから日時の名前へ移す。途中で失敗した書き出しが、完成した
/// 書き出しと同じ形の名前で残らないようにするため。
fn write(snapshot: &Snapshot, store: &AttachmentStore, root: &Path) -> Result<ExportSummary> {
    fs::create_dir_all(root).map_err(io_error("create the export folder"))?;
    let stamp = now_iso8601().replace(':', "-");
    let temp = root.join(format!(".{stamp}.{}.tmp", Ulid::new()));
    fs::create_dir(&temp).map_err(io_error("create the export folder"))?;

    let finished = write_files(snapshot, store, &temp).and_then(|(attachments, missing)| {
        Ok(ExportSummary {
            folder: move_into_place(&temp, root, &stamp)?,
            tasks: snapshot.tasks.len(),
            attachments,
            missing_attachments: missing,
        })
    });
    if finished.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    finished
}

/// 書いた添付の数と、書けなかった添付の数を返す。
fn write_files(snapshot: &Snapshot, store: &AttachmentStore, dir: &Path) -> Result<(usize, usize)> {
    let mut counts = (0, 0);
    let general = entries(&snapshot.general, store, dir, &mut counts);
    write_file(
        &dir.join("general-chat.md"),
        &markdown::render_general_chat(&general),
    )?;
    for (task, steps, chat) in &snapshot.tasks {
        let conversation = entries(chat, store, dir, &mut counts);
        let body = markdown::render_task(task, steps, &conversation);
        write_file(&dir.join(task_file_name(task)), &body)?;
    }
    Ok(counts)
}

/// 会話の行に、書き出した添付へのリンクを添える。
fn entries<'a>(
    chat: &'a ChatRows,
    store: &AttachmentStore,
    dir: &Path,
    (written, missing): &mut (usize, usize),
) -> Vec<Entry<'a>> {
    chat.messages
        .iter()
        .map(|message| {
            let attachments = chat
                .attachments
                .get(&message.id)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|attachment| {
                    let path = write_attachment(attachment, store, dir);
                    if path.is_some() {
                        *written += 1;
                    } else {
                        *missing += 1;
                    }
                    AttachmentLink {
                        name: attachment.view.original_name.clone(),
                        kind: attachment.view.kind,
                        mime_type: attachment.view.mime_type.clone(),
                        size_bytes: attachment.view.size_bytes,
                        path,
                    }
                })
                .collect();
            Entry {
                message,
                attachments,
            }
        })
        .collect()
}

/// 添付を`attachments/<添付のID>/`に元の名前(どのOSでも置ける形にしたもの)で書き、その相対パスの
/// 区間を返す。テキストの添付はDBの本文から書き戻す。実体を読めない・書けない添付は`None`にし、
/// 書き出し全体は止めない(残りのデータの持ち出しを優先する)。
fn write_attachment(
    attachment: &Attachment,
    store: &AttachmentStore,
    dir: &Path,
) -> Option<Vec<String>> {
    let segments = vec![
        ATTACHMENTS_DIR.to_string(),
        attachment.view.id.to_string(),
        safe_file_name(&attachment.view.original_name),
    ];
    let file = segments.iter().fold(dir.to_path_buf(), |p, s| p.join(s));
    match &attachment.content {
        AttachmentContent::Text(text) => write_bytes(&file, text.as_bytes()).ok()?,
        AttachmentContent::File { hash } => {
            create_parent(&file).ok()?;
            if store.copy_to(hash, &file).is_err() {
                // 同梱できなかった添付の入れ物を空のまま残さない(添付ごとのフォルダなので空)。
                if let Some(parent) = file.parent() {
                    let _ = fs::remove_dir(parent);
                }
                return None;
            }
        }
    }
    Some(segments)
}

/// `task-<ID>-<タイトル>.md`。IDで一意になり、タイトルは一覧で見分けるためだけに付ける。
fn task_file_name(task: &Task) -> String {
    let stem = match &task.title {
        Some(title) => {
            let title: String = title.chars().take(TITLE_CHARS_IN_FILE_NAME).collect();
            safe_file_name(&format!("task-{}-{title}", task.id))
        }
        None => format!("task-{}", task.id),
    };
    format!("{stem}.md")
}

/// 一時フォルダを`root/<stamp>`へ移す。同じ秒の書き出しが既にあれば`-2`・`-3`…を付ける。
/// 移し先を確かめてから移すまでの間に別の書き出しが同じ名前を取ることがあるので、移せなかった
/// ときも名前が埋まっていれば次の番号を試す(中身のあるフォルダへは移せないので上書きはしない)。
fn move_into_place(temp: &Path, root: &Path, stamp: &str) -> Result<String> {
    for n in 1.. {
        let name = if n == 1 {
            stamp.to_string()
        } else {
            format!("{stamp}-{n}")
        };
        let target = root.join(&name);
        if target.exists() {
            continue;
        }
        match fs::rename(temp, &target) {
            Ok(()) => return Ok(name),
            Err(_) if target.exists() => continue,
            Err(e) => return Err(io_error("finish the export folder")(e)),
        }
    }
    unreachable!("the folder name counter is unbounded")
}

fn write_file(path: &Path, body: &str) -> Result<()> {
    write_bytes(path, body.as_bytes())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    create_parent(path)?;
    fs::write(path, bytes).map_err(io_error("write an export file"))
}

fn create_parent(path: &Path) -> Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent).map_err(io_error("create an export folder")),
        None => Ok(()),
    }
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> CoreError {
    move |e| CoreError::Export(crate::files::describe_io_error(action, &e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::TempStore;
    use crate::db;
    use crate::db::attachments::{AttachmentKind, NewAttachment};
    use crate::db::messages::{Kind, NewMessage, Origin, Role};
    use crate::db::tasks::{TaskStatus, TaskUpdate};

    struct Fixture {
        temp: TempStore,
        conn: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                temp: TempStore::new(),
                conn: db::open_in_memory().unwrap(),
            }
        }

        fn task(&self, title: &str) -> i64 {
            let id = tasks::create_task(&self.conn).unwrap().id;
            let update = TaskUpdate {
                title: Some(title.to_string()),
                ..Default::default()
            };
            tasks::update_task(&self.conn, id, update).unwrap();
            id
        }

        fn message(
            &self,
            chat: Chat,
            role: Role,
            kind: Kind,
            content: &str,
            origin: Origin,
        ) -> i64 {
            messages::insert_message(
                &self.conn,
                NewMessage {
                    chat,
                    role,
                    content,
                    kind,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
            .unwrap()
        }

        fn say(&self, chat: Chat, content: &str) -> i64 {
            self.message(chat, Role::User, Kind::Normal, content, Origin::User)
        }

        fn reply(&self, chat: Chat, content: &str, turn_id: &str, attempt_no: i64) -> i64 {
            let origin = Origin::Turn {
                turn_id,
                attempt_no,
            };
            self.message(chat, Role::Assistant, Kind::Normal, content, origin)
        }

        fn attach(&self, message_id: i64, name: &str, content: AttachmentContent) {
            let kind = match content {
                AttachmentContent::Text(_) => AttachmentKind::Text,
                AttachmentContent::File { .. } => AttachmentKind::Image,
            };
            let new = NewAttachment {
                original_name: name.to_string(),
                mime_type: "application/octet-stream".to_string(),
                kind,
                size_bytes: 1,
                content,
            };
            attachments::insert(&self.conn, message_id, &new).unwrap();
        }

        fn export(&self) -> (ExportSummary, PathBuf) {
            let snapshot = Snapshot::read(&self.conn).unwrap();
            let out = self.temp.root().join("export");
            let summary = write(&snapshot, &self.temp.store, &out).unwrap();
            let folder = out.join(&summary.folder);
            (summary, folder)
        }
    }

    fn read(path: PathBuf) -> String {
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn writes_only_what_the_screen_shows() {
        let f = Fixture::new();
        let kept = f.task("残す");
        let chat = Chat::Task(kept);
        f.say(chat, "visible question");
        f.reply(chat, "old attempt", "turn-1", 1);
        f.reply(chat, "latest attempt", "turn-1", 2);
        let deleted = f.say(chat, "deleted message");
        messages::soft_delete_message(&f.conn, deleted).unwrap();
        // 返信の消えたターンの実行記録は、画面の会話から外れる。
        let discarded = Origin::Turn {
            turn_id: "turn-2",
            attempt_no: 1,
        };
        f.message(
            chat,
            Role::Tool,
            Kind::ToolExecution,
            r#"{"tool":"discarded"}"#,
            discarded,
        );

        let archived = f.task("しまった");
        let update = TaskUpdate {
            status: Some(TaskStatus::Archived),
            ..Default::default()
        };
        tasks::update_task(&f.conn, archived, update).unwrap();
        let removed = f.task("消した");
        tasks::delete_task(&f.conn, removed).unwrap();
        f.say(Chat::General, "general hello");

        let (summary, folder) = f.export();
        assert_eq!(summary.tasks, 2);
        let task = read(folder.join(format!("task-{kept}-残す.md")));
        assert!(task.contains("visible question"));
        assert!(task.contains("latest attempt"));
        assert!(!task.contains("old attempt"));
        assert!(!task.contains("deleted message"));
        assert!(!task.contains("discarded"));
        assert!(read(folder.join(format!("task-{archived}-しまった.md")))
            .contains("- Status: archived"));
        assert!(!folder.join(format!("task-{removed}-消した.md")).exists());
        assert!(read(folder.join("general-chat.md")).contains("general hello"));
    }

    #[test]
    fn copies_attachments_and_keeps_going_without_missing_files() {
        let f = Fixture::new();
        let chat = Chat::Task(f.task("添付"));
        let m = f.say(chat, "see these");
        f.attach(m, "notes.txt", AttachmentContent::Text("本文".to_string()));
        let hash = f.temp.store.put(b"\x89PNG\r\n\x1a\nbody").unwrap();
        f.attach(m, "a b.png", AttachmentContent::File { hash });
        f.attach(
            m,
            "gone.png",
            AttachmentContent::File {
                hash: "0".repeat(64),
            },
        );

        let (summary, folder) = f.export();
        assert_eq!((summary.attachments, summary.missing_attachments), (2, 1));
        let files = folder.join(ATTACHMENTS_DIR);
        let ids: Vec<i64> = f
            .conn
            .prepare("SELECT id FROM attachments ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            read(files.join(ids[0].to_string()).join("notes.txt")),
            "本文"
        );
        assert_eq!(
            fs::read(files.join(ids[1].to_string()).join("a b.png")).unwrap(),
            b"\x89PNG\r\n\x1a\nbody"
        );
        assert!(!files.join(ids[2].to_string()).exists());
        let task = read(folder.join("task-1-添付.md"));
        assert!(
            task.contains(&format!("](attachments/{}/a%20b.png)", ids[1])),
            "{task}"
        );
        assert!(
            task.contains("gone\\.png (application/octet-stream, size in bytes: 1, not exported)")
        );
    }

    #[test]
    fn each_export_gets_its_own_folder_and_leaves_no_temporary_folder() {
        let f = Fixture::new();
        let (first, _) = f.export();
        let (second, _) = f.export();
        assert_ne!(first.folder, second.folder);
        assert!(!first.folder.contains(':'));
        let names: Vec<String> = fs::read_dir(f.temp.root().join("export"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn task_file_names_carry_a_short_safe_title() {
        let mut task = tasks::Task {
            id: 7,
            title: None,
            description: None,
            deadline: None,
            archived_at: None,
            deleted_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert_eq!(task_file_name(&task), "task-7.md");
        task.title = Some("a/b: c?".to_string());
        assert_eq!(task_file_name(&task), "task-7-a_b_ c_.md");
        task.title = Some("あ".repeat(100));
        assert_eq!(
            task_file_name(&task),
            format!("task-7-{}.md", "あ".repeat(TITLE_CHARS_IN_FILE_NAME))
        );
    }
}
