//! 添付ファイル。種別の判定・実体の置き場所・送信前の添付と、画面が添付を開くときの
//! 読み出しをここに閉じる。行の読み書きは`db::attachments`。

mod classify;
mod dropped;
mod normalize;
mod staging;
mod store;

use std::path::PathBuf;

use rusqlite::Connection;
use serde::Serialize;

pub use classify::{classify, image_mime_type, Classified, Limits, PickingLimits, LIMITS};
pub use dropped::DropNotice;
pub(crate) use staging::Taken;
pub use staging::{Rejection, StageOutcome};
pub(crate) use store::safe_file_name;
#[cfg(test)]
pub(crate) use store::TempStore;
pub use store::{AttachmentStore, StoredBlob};

use crate::blocking;
use crate::db::attachments::{self, AttachmentContent, AttachmentKind, NewAttachment};
use crate::db::{with_conn, SharedConnection};
use crate::error::{CoreError, Result};

/// 添付をモデルへどう渡すか。履歴の組み立てと、画面が添付に出す警告
/// (`settings::ChatModelsView`)の両方がこれで決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// テキストの本文を送る。
    Content,
    /// 画像を一緒に送る。
    Image,
    /// 名前・種類・大きさだけを送る。
    NameOnly,
}

/// テキストは毎ターン全文を送る(履歴の間引きが予算を守る)。画像は直近のユーザー発言の
/// ものだけを、画像に対応するモデルへ送る(古い画像まで送るとコンテキストを使い続けるため)。
/// その他は中身を送らない。
pub fn delivery(kind: AttachmentKind, image_input: bool, in_latest_message: bool) -> Delivery {
    match kind {
        AttachmentKind::Text => Delivery::Content,
        AttachmentKind::Image if image_input && in_latest_message => Delivery::Image,
        AttachmentKind::Image | AttachmentKind::Other => Delivery::NameOnly,
    }
}

/// モデルが決まっていなくても決まる、これから送る発言の添付の渡し方。モデルの能力で変わる
/// 種別(画像)は`None`。モデルを選ぶ前から、画面が添付に警告を出せるようにするため。
pub fn delivery_without_model(kind: AttachmentKind) -> Option<Delivery> {
    let without = delivery(kind, false, true);
    (without == delivery(kind, true, true)).then_some(without)
}

/// アプリの起動中ずっと1つを使う。
pub struct Attachments {
    store: AttachmentStore,
    staged: staging::Staged,
    dropped: dropped::Dropped,
}

impl Attachments {
    pub fn new(store: AttachmentStore) -> Self {
        Self {
            store,
            staged: staging::Staged::default(),
            dropped: dropped::Dropped::default(),
        }
    }

    /// 選んだファイルを判定し、受け付けたら送信まで預かる。`name`は元のファイル名で、
    /// DBにそのまま残す。
    pub fn stage(&self, name: String, bytes: Vec<u8>) -> Result<StageOutcome> {
        self.staged.stage(name, bytes)
    }

    /// 窓に落とされたファイルを受け取り、画面へ知らせる形にする。パスはOSのドロップからGUIの
    /// シェルへ届いたもので、WebViewからは受け取らない(`docs/spec/architecture/attachments.md`
    /// 「受け取り方」)。ここでは読まない。
    pub fn receive_drop(&self, paths: Vec<PathBuf>) -> Option<DropNotice> {
        self.dropped.receive(paths)
    }

    /// 落としたファイルのうち、画面が受け付けたものを読み、[`Self::stage`]と同じく判定して
    /// 預ける。ファイルを読むのでブロッキング処理として呼ぶ。
    pub fn stage_dropped(&self, drop_id: u64, index: usize) -> Result<StageOutcome> {
        let path = self
            .dropped
            .take(drop_id, index)
            .ok_or_else(|| CoreError::Attachment("dropped file not found".to_string()))?;
        match dropped::read(&path)? {
            Ok(bytes) => self.stage(dropped::name_of(&path), bytes),
            Err(reason) => Ok(StageOutcome::Rejected { reason }),
        }
    }

    pub fn discard(&self, token: &str) {
        self.staged.discard(token);
    }

    pub(crate) fn take_staged(&self, tokens: &[String]) -> Result<Taken> {
        self.staged.take(tokens)
    }

    pub(crate) fn restore_staged(&self, taken: Taken) {
        self.staged.restore(taken);
    }

    /// 実体の置き場所。ブロッキング処理へ持ち出すための写し(パスだけを持つ)。
    pub(crate) fn store(&self) -> AttachmentStore {
        self.store.clone()
    }

    /// 取り出した添付の実体を書き、行として書く形にする(ファイルI/Oを伴う)。
    pub(crate) async fn store_taken(&self, taken: Taken) -> Result<Vec<NewAttachment>> {
        let store = self.store.clone();
        blocking::run(move || taken.store(&store)).await
    }

    pub fn read_text(&self, conn: &Connection, id: i64) -> Result<String> {
        match attachments::get(conn, id)?.content {
            AttachmentContent::Text(text) => Ok(text),
            AttachmentContent::File { .. } => Err(not_of_kind(id, AttachmentKind::Text)),
        }
    }

    /// 画像の添付を画面に出すdata URL。MIMEは保存した値ではなく実体の先頭バイトから決め直し、
    /// 画像として扱う形式でなければ作らない。data URLの先頭が必ず`data:image/…;base64,`に
    /// なり、画面で別の種類のURLに化けないようにするため(CSPの`img-src data:`の範囲)。
    pub async fn image_data_url(&self, db: SharedConnection, id: i64) -> Result<String> {
        let attachment = with_conn(db, move |conn| attachments::get(conn, id)).await?;
        let hash = file_hash(attachment.content, id, AttachmentKind::Image)?;
        let store = self.store.clone();
        blocking::run(move || Ok(store.read_image(&hash)?.data_url().to_string())).await
    }

    /// 添付の入ったフォルダを開く([`AttachmentStore::reveal`])。
    pub async fn reveal(&self, db: SharedConnection, id: i64) -> Result<()> {
        let attachment = with_conn(db, move |conn| attachments::get(conn, id)).await?;
        let hash = file_hash(attachment.content, id, AttachmentKind::Other)?;
        let store = self.store.clone();
        blocking::run(move || store.reveal(id, &attachment.view.original_name, &hash)).await
    }

    /// どの添付の行からも指されていない実体(`docs/spec/data-model/tables.md` attachments)。
    /// `delete`なら消し、消したものを返す。
    ///
    /// 実体の一覧を先に取り、行はその後に読む。逆にすると、行を読んだあとに送信された添付の
    /// 実体まで孤立して見える。それでも送信は実体を置いてから行を書くので、その間に走ると
    /// 送信中の添付の実体を消しうる。別のプロセスが添付を送信していないときに呼ぶこと。
    pub async fn orphaned_blobs(
        &self,
        db: SharedConnection,
        delete: bool,
    ) -> Result<Vec<StoredBlob>> {
        let store = self.store.clone();
        let stored = blocking::run(move || store.list()).await?;
        let referenced = with_conn(db, attachments::file_hashes).await?;
        let orphans: Vec<StoredBlob> = stored
            .into_iter()
            .filter(|blob| !referenced.contains(&blob.hash))
            .collect();
        if delete {
            let store = self.store.clone();
            let targets = orphans.clone();
            blocking::run(move || targets.iter().try_for_each(|blob| store.remove(&blob.hash)))
                .await?;
        }
        Ok(orphans)
    }
}

/// 実体を置き場所に持つ添付のハッシュ。行は先に引き終えておき、DBのロックを実体の読み書きへ
/// 持ち込まない。`expected`は、テキストの添付だったときの失敗の文言に使う。
fn file_hash(content: AttachmentContent, id: i64, expected: AttachmentKind) -> Result<String> {
    match content {
        AttachmentContent::File { hash } => Ok(hash),
        AttachmentContent::Text(_) => Err(not_of_kind(id, expected)),
    }
}

fn not_of_kind(id: i64, kind: AttachmentKind) -> CoreError {
    CoreError::Attachment(format!("attachment {id} is not a {kind:?} attachment"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::db;
    use crate::db::messages::{self, Chat, Kind, NewMessage, Origin, Role};

    /// 実体を置き、それを指す添付の行を書く。
    fn attach(conn: &Connection, store: &AttachmentStore, bytes: &[u8]) -> String {
        let hash = store.put(bytes).unwrap();
        let message_id = messages::insert_message(
            conn,
            NewMessage {
                chat: Chat::General,
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
        let new = NewAttachment {
            original_name: "file.bin".to_string(),
            mime_type: "application/octet-stream".to_string(),
            kind: AttachmentKind::Other,
            size_bytes: bytes.len() as i64,
            content: AttachmentContent::File { hash: hash.clone() },
        };
        attachments::insert(conn, message_id, &new).unwrap();
        hash
    }

    #[tokio::test]
    async fn only_blobs_no_row_points_at_are_orphans_and_only_they_are_deleted() {
        let t = TempStore::new();
        let conn = db::open_in_memory().unwrap();
        let kept = attach(&conn, &t.store, b"kept");
        let orphan = t.store.put(b"orphan").unwrap();
        // 実体の名前の形をしていないファイルには触れない。
        let stray = t.root().join("blobs").join("notes.txt");
        std::fs::write(&stray, b"stray").unwrap();
        let db = Arc::new(Mutex::new(conn));
        let attachments = Attachments::new(t.store.clone());

        let expected = vec![StoredBlob {
            hash: orphan.clone(),
            size_bytes: 6,
        }];
        let listed = attachments.orphaned_blobs(db.clone(), false).await.unwrap();
        assert_eq!(listed, expected);
        assert!(t.store.read(&orphan).is_ok(), "一覧だけでは消さない");

        let deleted = attachments.orphaned_blobs(db.clone(), true).await.unwrap();
        assert_eq!(deleted, expected);
        assert!(t.store.read(&orphan).is_err());
        assert!(t.store.read(&kept).is_ok());
        assert!(stray.exists());
        assert_eq!(attachments.orphaned_blobs(db, true).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn a_store_that_does_not_exist_yet_has_no_orphans() {
        let t = TempStore::new();
        let db = Arc::new(Mutex::new(db::open_in_memory().unwrap()));
        let attachments = Attachments::new(t.store.clone());

        assert_eq!(attachments.orphaned_blobs(db, true).await.unwrap(), vec![]);
    }

    #[test]
    fn stages_a_dropped_file_under_its_own_name_and_refuses_folders() {
        let t = TempStore::new();
        let attachments = Attachments::new(t.store.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo.txt");
        std::fs::write(&path, "hello").unwrap();

        let notice = attachments
            .receive_drop(vec![path, dir.path().to_path_buf()])
            .unwrap();
        assert_eq!(notice.names[0], "memo.txt");
        let StageOutcome::Staged { token, kind, .. } =
            attachments.stage_dropped(notice.drop_id, 0).unwrap()
        else {
            panic!("expected staged");
        };
        assert_eq!(kind, AttachmentKind::Text);
        assert_eq!(attachments.take_staged(&[token]).unwrap().len(), 1);
        assert_eq!(
            attachments.stage_dropped(notice.drop_id, 1).unwrap(),
            StageOutcome::Rejected {
                reason: Rejection::NotAFile
            }
        );
        assert!(attachments.stage_dropped(notice.drop_id, 0).is_err());
    }
}
