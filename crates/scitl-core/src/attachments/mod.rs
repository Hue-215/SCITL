//! 添付ファイル(Issue #21)。種別の判定・実体の置き場所・送信前の添付と、画面が添付を
//! 開くときの読み出しをここに閉じる。行の読み書きは`db::attachments`。

mod classify;
mod staging;
mod store;

use base64::Engine;
use rusqlite::Connection;

pub use classify::{classify, image_mime_type, Classified, Limits, LIMITS};
pub use staging::{Rejection, StageOutcome};
pub use store::AttachmentStore;

use crate::blocking;
use crate::db::attachments::{self, AttachmentContent, AttachmentKind, NewAttachment};
use crate::db::error::{CoreError, Result};
use crate::db::{with_conn, SharedConnection};

/// アプリの起動中ずっと1つを使う。
pub struct Attachments {
    store: AttachmentStore,
    staged: staging::Staged,
}

impl Attachments {
    pub fn new(store: AttachmentStore) -> Self {
        Self {
            store,
            staged: staging::Staged::default(),
        }
    }

    /// 選んだファイルを判定し、受け付けたら送信まで預かる。`name`は元のファイル名で、
    /// DBにそのまま残す。
    pub fn stage(&self, name: String, bytes: &[u8]) -> Result<StageOutcome> {
        self.staged.stage(&self.store, name, bytes)
    }

    pub fn discard(&self, token: &str) {
        self.staged.discard(token);
    }

    pub(crate) fn resolve_staged(&self, tokens: &[String]) -> Result<Vec<NewAttachment>> {
        self.staged.resolve(tokens)
    }

    pub(crate) fn remove_staged(&self, tokens: &[String]) {
        self.staged.remove(tokens);
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
        blocking::run(move || {
            let bytes = store.read(&hash)?;
            let mime_type =
                image_mime_type(&bytes).ok_or_else(|| not_of_kind(id, AttachmentKind::Image))?;
            Ok(format!(
                "data:{mime_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ))
        })
        .await
    }

    /// 添付の入ったフォルダを開く([`AttachmentStore::reveal`])。
    pub async fn reveal(&self, db: SharedConnection, id: i64) -> Result<()> {
        let attachment = with_conn(db, move |conn| attachments::get(conn, id)).await?;
        let hash = file_hash(attachment.content, id, AttachmentKind::Other)?;
        let store = self.store.clone();
        blocking::run(move || store.reveal(id, &attachment.view.original_name, &hash)).await
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
