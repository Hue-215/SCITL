//! Markdownエクスポート。タスクと総合チャットを、それぞれ1つのMarkdownファイルに書き出す。
//! 全部をまとめて書き出すか、会話1つだけを書き出すかは[`ExportScope`]で選ぶ。
//! 書き出すのは画面から見られるものだけで、削除したタスク・工程・発言と、会話から外れた
//! ターン(古い試行・破棄されたターン)は含めない。思考と`error_detail`も含めない。失敗した
//! ターンで受け取り終えた中身(エラー発言の`parts`)は、画面と同じく含める。
//!
//! 書き出し先は呼び出し側が決める。画面からはパスを受け取らない。どこへ書くか(データディレクトリの
//! フォルダか、利用者が選んだファイルか)は[`ExportTarget::CURRENT`]で決める。

mod archive;
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
use crate::db::memories::{self, Memory};
use crate::db::messages::{self, Chat, Message, ReplyRecords};
use crate::db::task_steps::{self, TaskStep};
use crate::db::tasks::{self, Task};
use crate::db::{now_iso8601, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::text;
use markdown::{AttachmentLink, Entry};

pub use archive::{export_to_chosen_file, OpenChosen};

const ATTACHMENTS_DIR: &str = "attachments";
/// ファイル名に入れるタイトルの長さ。フォルダまでのパスと合わせて、Windowsのパスの長さの
/// 上限に届かないよう短めにする(タイトルの全体はファイルの中にある)。
const TITLE_CHARS_IN_FILE_NAME: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// 全タスク・総合チャット・メモリ。
    All,
    /// その会話だけ(タスクなら説明と工程を含む)。メモリは含めない。
    Chat(Chat),
}

impl ExportScope {
    /// フォルダの名前の日時に続けて付ける、何を書き出したか。
    fn folder_suffix(self) -> Option<String> {
        match self {
            Self::All => None,
            Self::Chat(Chat::General) => Some("general-chat".to_string()),
            Self::Chat(Chat::Task(id)) => Some(format!("task-{id}")),
        }
    }
}

/// 書き出す先。画面は受け取って、押せる操作と結果の文言を出し分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ExportTarget {
    /// データディレクトリの`export/`に、日時の名前のフォルダを作る([`export_markdown`])。画面は
    /// そのフォルダを開ける([`open_folder`])。
    Folder,
    /// 利用者が保存画面で選んだファイルへ、そのフォルダをzipにまとめて書く
    /// ([`export_to_chosen_file`])。データディレクトリがほかのアプリから見えず、フォルダを開く
    /// 手段も無いAndroidで使う。
    ChosenFile,
}

impl ExportTarget {
    /// このOSで書き出す先。
    pub const CURRENT: Self = if cfg!(target_os = "android") {
        Self::ChosenFile
    } else {
        Self::Folder
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExportOutcome {
    /// データディレクトリの`export/`のフォルダへ書いた([`ExportTarget::Folder`])。
    Written { summary: ExportSummary },
    /// 選んだファイルへzipで書いた([`ExportTarget::ChosenFile`])。`summary.folder`はzipの中の
    /// フォルダの名前。
    Saved { summary: ExportSummary },
    /// 保存画面を閉じた。
    Cancelled,
    /// 選んだファイルへ書き写せなかった。選んだファイルは保存画面が作るので、書きかけ(空を含む)が
    /// 選んだ名前で残りうる。消す手段を持たないので、画面は利用者に消すよう伝える。
    LeftIncomplete { reason: String },
}

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

/// `root`の下に書き出した日時の名前のフォルダを作り、その中へ`scope`を書き出す。前回までの
/// 書き出しには触れない。
///
/// DBは1回のロックの中で全部を読み、ファイルの書き込みにはロックを持ち込まない。
pub async fn export_markdown(
    db: SharedConnection,
    attachments: &Attachments,
    root: PathBuf,
    scope: ExportScope,
) -> Result<ExportSummary> {
    let snapshot = with_conn(db, move |conn| Snapshot::read(conn, scope)).await?;
    let store = attachments.store();
    blocking::run(move || write(&snapshot, &store, &root)).await
}

/// 書き出し先のフォルダをOSで開く。まだ一度も書き出していなければ空のフォルダを作って開く。
/// フォルダへ書き出さないOS([`ExportTarget::CURRENT`])では、作る前に断る。
pub fn open_folder(root: &Path) -> Result<()> {
    if ExportTarget::CURRENT != ExportTarget::Folder {
        return Err(CoreError::Export(
            "exports are not written to a folder on this platform".to_string(),
        ));
    }
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

/// 書き出す範囲のデータ。範囲に入らない総合チャット・メモリは`None`にし、そのファイルを書かない。
struct Snapshot {
    tasks: Vec<(Task, Vec<TaskStep>, ChatRows)>,
    general: Option<ChatRows>,
    memories: Option<Vec<Memory>>,
    folder_suffix: Option<String>,
}

impl Snapshot {
    /// 削除したタスクを指すと`TaskNotFound`で断る。
    fn read(conn: &Connection, scope: ExportScope) -> Result<Self> {
        let tasks = match scope {
            ExportScope::All => tasks::list_all(conn)?,
            ExportScope::Chat(Chat::Task(id)) => vec![tasks::get_task(conn, id)?],
            ExportScope::Chat(Chat::General) => Vec::new(),
        }
        .into_iter()
        .map(|task| {
            let steps = task_steps::list_for_task(conn, task.id)?;
            let chat = ChatRows::read(conn, Chat::Task(task.id))?;
            Ok((task, steps, chat))
        })
        .collect::<Result<Vec<_>>>()?;
        let general = match scope {
            ExportScope::All | ExportScope::Chat(Chat::General) => {
                Some(ChatRows::read(conn, Chat::General)?)
            }
            ExportScope::Chat(Chat::Task(_)) => None,
        };
        let memories = match scope {
            ExportScope::All => Some(memories::list(conn)?),
            ExportScope::Chat(_) => None,
        };
        Ok(Self {
            tasks,
            general,
            memories,
            folder_suffix: scope.folder_suffix(),
        })
    }
}

/// 隠しの一時フォルダに書いてから日時の名前へ移す。途中で失敗した書き出しが、完成した
/// 書き出しと同じ形の名前で残らないようにするため。
fn write(snapshot: &Snapshot, store: &AttachmentStore, root: &Path) -> Result<ExportSummary> {
    fs::create_dir_all(root).map_err(io_error("create the export folder"))?;
    let stamp = now_iso8601().replace(':', "-");
    let name = match &snapshot.folder_suffix {
        Some(suffix) => format!("{stamp}-{suffix}"),
        None => stamp,
    };
    let temp = root.join(format!(".{name}.{}.tmp", Ulid::new()));
    fs::create_dir(&temp).map_err(io_error("create the export folder"))?;

    let finished = write_files(snapshot, store, &temp).and_then(|(attachments, missing)| {
        Ok(ExportSummary {
            folder: move_into_place(&temp, root, &name)?,
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
    if let Some(general) = &snapshot.general {
        let general = entries(general, store, dir, &mut counts);
        write_file(
            &dir.join("general-chat.md"),
            &markdown::render_general_chat(&general),
        )?;
    }
    if let Some(memories) = &snapshot.memories {
        write_file(
            &dir.join("memories.md"),
            &markdown::render_memories(memories),
        )?;
    }
    for (task, steps, chat) in &snapshot.tasks {
        let conversation = entries(chat, store, dir, &mut counts);
        let body = markdown::render_task(task, steps, &conversation);
        write_file(&dir.join(task_file_name(task)), &body)?;
    }
    Ok(counts)
}

/// 会話の行に、書き出した添付へのリンクを添える。返信の中身が指す実行記録は、独立した項目に
/// せず返信の項目の中に並べる。
fn entries<'a>(
    chat: &'a ChatRows,
    store: &AttachmentStore,
    dir: &Path,
    (written, missing): &mut (usize, usize),
) -> Vec<Entry<'a>> {
    let records = ReplyRecords::of(&chat.messages);
    chat.messages
        .iter()
        .filter(|message| !records.contains(message.id))
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
                parts: records.resolve(message),
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
            let (title, _) = text::truncate_chars(title, TITLE_CHARS_IN_FILE_NAME);
            safe_file_name(&format!("task-{}-{title}", task.id))
        }
        None => format!("task-{}", task.id),
    };
    format!("{stem}.md")
}

/// 一時フォルダを`root/<name>`へ移す。同じ秒の同じ範囲の書き出しが既にあれば`-2`・`-3`…を付ける。
/// 移し先を確かめてから移すまでの間に別の書き出しが同じ名前を取ることがあるので、移せなかった
/// ときも名前が埋まっていれば次の番号を試す(中身のあるフォルダへは移せないので上書きはしない)。
fn move_into_place(temp: &Path, root: &Path, name: &str) -> Result<String> {
    for n in 1.. {
        let name = if n == 1 {
            name.to_string()
        } else {
            format!("{name}-{n}")
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
                    parts: (matches!(origin, Origin::Turn { .. }) && kind == Kind::Normal)
                        .then_some(&[]),
                },
            )
            .unwrap()
        }

        fn say(&self, chat: Chat, content: &str) -> i64 {
            self.message(chat, Role::User, Kind::Normal, content, Origin::User)
        }

        fn reply(&self, chat: Chat, content: &str, turn_id: &str, attempt_no: i64) -> i64 {
            let parts = messages::parts_for_reply(&self.conn, turn_id, attempt_no, Some(content));
            messages::insert_message(
                &self.conn,
                NewMessage {
                    chat,
                    role: Role::Assistant,
                    content: "",
                    kind: Kind::Normal,
                    origin: Origin::Turn {
                        turn_id,
                        attempt_no,
                    },
                    error_kind: None,
                    error_detail: None,
                    parts: Some(&parts),
                },
            )
            .unwrap()
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
            self.export_scope(ExportScope::All)
        }

        fn export_scope(&self, scope: ExportScope) -> (ExportSummary, PathBuf) {
            let snapshot = Snapshot::read(&self.conn, scope).unwrap();
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
        // 返信の中身が指す記録は、返信の項目の中に、本文との起きた順で並ぶ。
        f.say(chat, "with a tool");
        f.message(
            chat,
            Role::Tool,
            Kind::ToolExecution,
            r#"{"tool":"list_tasks"}"#,
            Origin::Turn {
                turn_id: "turn-3",
                attempt_no: 1,
            },
        );
        f.reply(chat, "after the call", "turn-3", 1);

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
        let call = task.find("Tool execution (").unwrap();
        assert!(task.find("### User").unwrap() < call);
        assert!(call < task.find("after the call").unwrap());
        assert!(!task.contains("### Tool execution"));
        assert!(read(folder.join(format!("task-{archived}-しまった.md")))
            .contains("- Status: archived"));
        assert!(!folder.join(format!("task-{removed}-消した.md")).exists());
        assert!(read(folder.join("general-chat.md")).contains("general hello"));
    }

    /// 本文はMarkdownの構造として読まれない形にする。
    #[test]
    fn writes_memories_except_deleted_ones() {
        let f = Fixture::new();
        memories::add(&f.conn, &["朝型 *強調*".to_string(), "消した".to_string()]).unwrap();
        let removed = memories::list(&f.conn).unwrap()[1].id;
        memories::delete(&f.conn, removed).unwrap();

        let (_, folder) = f.export();

        let body = read(folder.join("memories.md"));
        assert!(body.contains("朝型 \\*強調\\*"));
        assert!(!body.contains("消した"));
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

    fn file_names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// アーカイブしたタスクも書ける。
    #[test]
    fn a_task_export_writes_only_that_task() {
        let f = Fixture::new();
        let kept = f.task("残す");
        let update = TaskUpdate {
            description: tasks::FieldChange::Set("the description".to_string()),
            status: Some(TaskStatus::Archived),
            ..Default::default()
        };
        tasks::update_task(&f.conn, kept, update).unwrap();
        task_steps::add_steps(&f.conn, kept, &["the step".to_string()]).unwrap();
        let m = f.say(Chat::Task(kept), "mine");
        f.attach(m, "mine.txt", AttachmentContent::Text("本文".to_string()));
        let other = f.task("ほか");
        let m = f.say(Chat::Task(other), "other");
        f.attach(m, "other.txt", AttachmentContent::Text("本文".to_string()));
        let m = f.say(Chat::General, "general");
        f.attach(
            m,
            "general.txt",
            AttachmentContent::Text("本文".to_string()),
        );
        memories::add(&f.conn, &["朝型".to_string()]).unwrap();

        let (summary, folder) = f.export_scope(ExportScope::Chat(Chat::Task(kept)));

        assert!(
            summary.folder.ends_with(&format!("-task-{kept}")),
            "{}",
            summary.folder
        );
        assert_eq!((summary.tasks, summary.attachments), (1, 1));
        assert_eq!(
            file_names(&folder),
            [ATTACHMENTS_DIR.to_string(), format!("task-{kept}-残す.md")]
        );
        assert_eq!(file_names(&folder.join(ATTACHMENTS_DIR)).len(), 1);
        let body = read(folder.join(format!("task-{kept}-残す.md")));
        for expected in ["the description", "the step", "mine", "- Status: archived"] {
            assert!(body.contains(expected), "{expected}: {body}");
        }
        assert!(!body.contains("other"));
    }

    /// 範囲の違う書き出しとは名前が重ならない。
    #[test]
    fn exports_of_the_same_scope_get_their_own_folders() {
        let f = Fixture::new();
        let id = f.task("同じ");
        let scope = ExportScope::Chat(Chat::Task(id));
        let (first, _) = f.export_scope(scope);
        let (second, _) = f.export_scope(scope);
        let (all, _) = f.export();
        assert_ne!(first.folder, second.folder);
        assert!(first.folder.ends_with(&format!("-task-{id}")));
        assert!(!all.folder.contains("task"));
    }

    #[test]
    fn a_general_chat_export_writes_only_the_general_chat() {
        let f = Fixture::new();
        f.say(Chat::Task(f.task("ほか")), "other");
        f.say(Chat::General, "general hello");
        memories::add(&f.conn, &["朝型".to_string()]).unwrap();

        let (summary, folder) = f.export_scope(ExportScope::Chat(Chat::General));

        assert!(
            summary.folder.ends_with("-general-chat"),
            "{}",
            summary.folder
        );
        assert_eq!(summary.tasks, 0);
        assert_eq!(file_names(&folder), ["general-chat.md"]);
        assert!(read(folder.join("general-chat.md")).contains("general hello"));
    }

    #[test]
    fn a_deleted_task_is_not_exported() {
        let f = Fixture::new();
        let removed = f.task("消した");
        tasks::delete_task(&f.conn, removed).unwrap();

        let result = Snapshot::read(&f.conn, ExportScope::Chat(Chat::Task(removed)));

        assert!(matches!(result, Err(CoreError::TaskNotFound(id)) if id == removed));
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
