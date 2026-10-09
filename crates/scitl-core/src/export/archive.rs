//! 書き出しを1つのzipにまとめ、利用者が保存画面で選んだファイルへ書き写す
//! ([`ExportTarget::ChosenFile`](super::ExportTarget))。保存画面で選べるのは1ファイルだけなので、
//! 日時の名前のフォルダ(デスクトップの書き出しと同じ形)をzipの中に置く。
//!
//! 選んだファイルは書き写す前に作られていて、消す手段を持たない。zipをキャッシュの中で作り終えて
//! から書き写し、書き写すのに失敗したら、書きかけが残りうることを結果で返す。

use std::fs::{self, File};
use std::io::{self, BufWriter, ErrorKind};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Datelike, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::{io_error, write, ExportOutcome, ExportSummary, Snapshot};
use crate::attachments::{AttachmentStore, Attachments};
use crate::blocking;
use crate::db::{with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::paths;

/// 選んだファイルを書き込み用に開く手順。保存画面が返したもの(パス・`content://`のURI)の開き方は
/// 呼び出し側が決める。
pub type OpenChosen = Box<dyn FnOnce() -> io::Result<File> + Send>;

/// zipを作っている間は真。同時に作るのは1つだけにする(作る場所を始めに片付けるため。乗っ取られた
/// 画面が並べて呼んでも、キャッシュにzipを積ませない)。
static BUILDING: AtomicBool = AtomicBool::new(false);

/// [`BUILDING`]を立てている間持つ。失敗して抜けても(panicを含む)外す。
struct Building;

impl Building {
    fn begin() -> Option<Self> {
        (!BUILDING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Building {
    fn drop(&mut self) {
        BUILDING.store(false, Ordering::Release);
    }
}

/// zipを`staging`(キャッシュの中)に作ってから`choose`で保存画面を出し、選ばれたファイルへ書き写す。
/// `choose`は勧めるファイル名を受け取り、取りやめたら`None`を返す。何を書くかは利用者が保存画面で
/// 選ぶので、選ばれない限りキャッシュの外には何も書かない。作ったzipは、成否によらず消す。
pub async fn export_to_chosen_file(
    db: SharedConnection,
    attachments: &Attachments,
    staging: PathBuf,
    choose: impl FnOnce(&str) -> Option<OpenChosen> + Send + 'static,
) -> Result<ExportOutcome> {
    let building = Building::begin()
        .ok_or_else(|| CoreError::Export("another export is in progress".to_string()))?;
    let snapshot = with_conn(db, Snapshot::read).await?;
    let store = attachments.store();
    blocking::run(move || {
        let _building = building;
        write_chosen_file(&snapshot, &store, &staging, choose)
    })
    .await
}

fn write_chosen_file(
    snapshot: &Snapshot,
    store: &AttachmentStore,
    staging: &Path,
    choose: impl FnOnce(&str) -> Option<OpenChosen>,
) -> Result<ExportOutcome> {
    let archive = Archive::build(snapshot, store, staging)?;
    let Some(open) = choose(&archive.file_name()) else {
        return Ok(ExportOutcome::Cancelled);
    };
    Ok(match archive.copy_into(open) {
        Ok(()) => ExportOutcome::Saved {
            summary: archive.summary.clone(),
        },
        Err(e) => ExportOutcome::LeftIncomplete {
            reason: crate::files::describe_io_error("write the chosen file", &e),
        },
    })
}

/// キャッシュの中に作ったzip。落とすと消す。
struct Archive {
    path: PathBuf,
    summary: ExportSummary,
}

impl Archive {
    /// 前に作りかけたもの(強制終了で残ったもの)を片付けてから、`staging`の中にフォルダを書き出し、
    /// zipにまとめる。zipに入れたファイルはその都度消す(キャッシュに中身を2つ分載せないため)。
    fn build(snapshot: &Snapshot, store: &AttachmentStore, staging: &Path) -> Result<Self> {
        match fs::remove_dir_all(staging) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(io_error("clear the export staging folder")(e))
            }
            _ => {}
        }
        paths::create_private_dir(staging).map_err(io_error("create the export staging folder"))?;
        let folders = staging.join("folders");
        let summary = write(snapshot, store, &folders)?;
        let archive = Self {
            path: staging.join(format!("{}.zip", summary.folder)),
            summary,
        };
        let zipped = zip_folder(
            &folders.join(&archive.summary.folder),
            &archive.summary.folder,
            &archive.path,
        );
        let _ = fs::remove_dir_all(&folders);
        zipped.map(|()| archive)
    }

    /// 保存画面に勧める名前。
    fn file_name(&self) -> String {
        format!("scitl-export-{}.zip", self.summary.folder)
    }

    /// 選んだファイルを開いて書き写す。開く手順がpanicしても(fsのAndroid側は、提供元がファイル
    /// 記述子を返さないとpanicする)失敗として返し、書きかけが残りうることを伝えられるようにする。
    fn copy_into(&self, open: OpenChosen) -> io::Result<()> {
        let path = &self.path;
        panic::catch_unwind(AssertUnwindSafe(move || {
            let mut out = open()?;
            io::copy(&mut File::open(path)?, &mut out)?;
            sync(&out)
        }))
        .unwrap_or_else(|_| Err(io::Error::other("opening the chosen file panicked")))
    }
}

/// 書き写した中身を記憶装置へ書き切らせる。提供元がパイプで受け取る場合は書き切る操作を持たない
/// ので、それは失敗にしない。
fn sync(out: &File) -> io::Result<()> {
    match out.sync_all() {
        Err(e) if matches!(e.kind(), ErrorKind::InvalidInput | ErrorKind::Unsupported) => Ok(()),
        result => result,
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// `folder`の中身を、`prefix`のフォルダに入れた形で`dest`のzipにまとめる。並びは名前順。
fn zip_folder(folder: &Path, prefix: &str, dest: &Path) -> Result<()> {
    let file = File::create(dest).map_err(io_error("create the export zip"))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(modified_now());
    add_folder(&mut zip, folder, prefix, options)?;
    zip.finish()
        .map_err(zip_error)?
        .into_inner()
        .map(drop)
        .map_err(|e| io_error("write the export zip")(e.into_error()))
}

fn add_folder(
    zip: &mut ZipWriter<BufWriter<File>>,
    folder: &Path,
    prefix: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    let read = io_error("read the export folder");
    let mut entries = fs::read_dir(folder)
        .map_err(&read)?
        .collect::<io::Result<Vec<_>>>()
        .map_err(&read)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        let metadata = entry.metadata().map_err(&read)?;
        if metadata.is_dir() {
            add_folder(zip, &path, &name, options)?;
            continue;
        }
        // 4GiBを超えるものはZIP64の形で書く(宣言しないと書く途中で失敗する)。
        let options = options.large_file(metadata.len() >= u64::from(u32::MAX));
        zip.start_file(name, options).map_err(zip_error)?;
        io::copy(&mut File::open(&path).map_err(&read)?, zip)
            .map_err(io_error("write the export zip"))?;
        let _ = fs::remove_file(&path);
    }
    Ok(())
}

/// zipの中のファイルの更新日時。zipの日時は時差を持たないので、ファイルアプリが表示する端末の
/// 時刻で書く。zipで表せない日時(1980年より前等)なら、zipの既定の日時のままにする。
fn modified_now() -> zip::DateTime {
    let now = chrono::Local::now();
    let parts = (
        u16::try_from(now.year()),
        u8::try_from(now.month()),
        u8::try_from(now.day()),
        u8::try_from(now.hour()),
        u8::try_from(now.minute()),
        u8::try_from(now.second()),
    );
    match parts {
        (Ok(y), Ok(mo), Ok(d), Ok(h), Ok(mi), Ok(s)) => {
            zip::DateTime::from_date_and_time(y, mo, d, h, mi, s).unwrap_or_default()
        }
        _ => zip::DateTime::default(),
    }
}

fn zip_error(e: zip::result::ZipError) -> CoreError {
    CoreError::Export(format!("could not write the export zip: {e}"))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::attachments::TempStore;
    use crate::db;
    use crate::db::attachments::{self, AttachmentContent, AttachmentKind, NewAttachment};
    use crate::db::messages::{self, Chat, Kind, NewMessage, Origin, Role};

    struct Fixture {
        temp: TempStore,
        snapshot: Snapshot,
    }

    impl Fixture {
        /// 総合チャットに、テキストと画像の添付を付けた発言が1つある。
        fn new() -> Self {
            let temp = TempStore::new();
            let conn = db::open_in_memory().unwrap();
            let message = messages::insert_message(
                &conn,
                NewMessage {
                    chat: Chat::General,
                    role: Role::User,
                    content: "hello",
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    parts: None,
                },
            )
            .unwrap();
            let hash = temp.store.put(b"\x89PNG\r\n\x1a\nbody").unwrap();
            for (name, kind, content) in [
                (
                    "notes.txt",
                    AttachmentKind::Text,
                    AttachmentContent::Text("本文".to_string()),
                ),
                (
                    "写真.png",
                    AttachmentKind::Image,
                    AttachmentContent::File { hash },
                ),
            ] {
                let new = NewAttachment {
                    original_name: name.to_string(),
                    mime_type: "application/octet-stream".to_string(),
                    kind,
                    size_bytes: 1,
                    content,
                };
                attachments::insert(&conn, message, &new).unwrap();
            }
            let snapshot = Snapshot::read(&conn).unwrap();
            Self { temp, snapshot }
        }

        fn staging(&self) -> PathBuf {
            self.temp.root().join("staging")
        }

        fn export(&self, choose: impl FnOnce(&str) -> Option<OpenChosen>) -> ExportOutcome {
            write_chosen_file(&self.snapshot, &self.temp.store, &self.staging(), choose).unwrap()
        }

        /// 選んだファイルの代わりに書く先。
        fn chosen(&self) -> PathBuf {
            self.temp.root().join("chosen.zip")
        }

        fn open_chosen(&self) -> Option<OpenChosen> {
            let path = self.chosen();
            Some(Box::new(move || File::create(path)))
        }
    }

    fn staged_files(staging: &Path) -> Vec<String> {
        fs::read_dir(staging)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn writes_the_dated_folder_into_the_chosen_file_and_leaves_nothing_behind() {
        let f = Fixture::new();
        let mut suggested = String::new();
        let outcome = f.export(|name| {
            suggested = name.to_string();
            f.open_chosen()
        });

        let ExportOutcome::Saved { summary } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(suggested, format!("scitl-export-{}.zip", summary.folder));
        assert_eq!((summary.attachments, summary.missing_attachments), (2, 0));
        let mut zip = zip::ZipArchive::new(File::open(f.chosen()).unwrap()).unwrap();
        let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
        names.sort();
        let folder = &summary.folder;
        let ids: Vec<String> = names
            .iter()
            .filter_map(|n| n.strip_prefix(&format!("{folder}/attachments/")))
            .map(|rest| rest.split('/').next().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                format!("{folder}/attachments/{}/notes.txt", ids[0]),
                format!("{folder}/attachments/{}/写真.png", ids[1]),
                format!("{folder}/general-chat.md"),
                format!("{folder}/memories.md"),
            ]
        );
        let mut body = String::new();
        zip.by_name(&format!("{folder}/general-chat.md"))
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(body.contains("hello"), "{body}");
        let mut image = Vec::new();
        zip.by_name(&format!("{folder}/attachments/{}/写真.png", ids[1]))
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        assert_eq!(image, b"\x89PNG\r\n\x1a\nbody");
        assert!(staged_files(&f.staging()).is_empty());
    }

    #[test]
    fn writes_nothing_outside_the_cache_when_the_save_dialog_is_closed() {
        let f = Fixture::new();
        assert!(matches!(f.export(|_| None), ExportOutcome::Cancelled));
        assert!(!f.chosen().exists());
        assert!(staged_files(&f.staging()).is_empty());
    }

    #[test]
    fn reports_that_the_chosen_file_may_be_left_incomplete() {
        let f = Fixture::new();
        let failing: OpenChosen = Box::new(|| Err(io::Error::other("provider went away")));
        let outcome = f.export(|_| Some(failing));
        let ExportOutcome::LeftIncomplete { reason } = outcome else {
            panic!("{outcome:?}");
        };
        assert!(reason.contains("provider went away"), "{reason}");
        assert!(staged_files(&f.staging()).is_empty());
    }

    #[test]
    fn reports_a_panicking_open_as_possibly_left_incomplete() {
        let f = Fixture::new();
        let panicking: OpenChosen = Box::new(|| panic!("no file descriptor"));
        assert!(matches!(
            f.export(|_| Some(panicking)),
            ExportOutcome::LeftIncomplete { .. }
        ));
        assert!(staged_files(&f.staging()).is_empty());
    }

    #[test]
    fn clears_what_an_earlier_export_left_in_the_staging_folder() {
        let f = Fixture::new();
        let leftover = f.staging().join("folders").join("half");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(f.staging().join("old.zip"), b"partial").unwrap();
        assert!(matches!(f.export(|_| None), ExportOutcome::Cancelled));
        assert!(staged_files(&f.staging()).is_empty());
    }

    #[test]
    fn only_one_archive_is_built_at_a_time() {
        let first = Building::begin().unwrap();
        assert!(Building::begin().is_none());
        drop(first);
        assert!(Building::begin().is_some());
    }
}
