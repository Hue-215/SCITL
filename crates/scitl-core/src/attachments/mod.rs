//! 添付ファイル(Issue #21)。種別の判定・実体の置き場所・送信前の添付と、画面が添付を
//! 開くときの読み出しをここに閉じる。行の読み書きは`db::attachments`。

mod classify;
mod normalize;
mod staging;
mod store;

use rusqlite::Connection;
use serde::Serialize;

pub use classify::{classify, image_mime_type, Classified, Limits, PickingLimits, LIMITS};
pub(crate) use staging::Taken;
pub use staging::{Rejection, StageOutcome};
pub(crate) use store::safe_file_name;
pub use store::AttachmentStore;
#[cfg(test)]
pub(crate) use store::TempStore;

use crate::blocking;
use crate::db::attachments::{self, AttachmentContent, AttachmentKind, NewAttachment};
use crate::db::{with_conn, SharedConnection};
use crate::error::{CoreError, Result};

/// 添付をモデルへどう渡すか(Issue #21)。履歴の組み立てと、画面が添付に出す警告
/// (`settings::ChatModelsView`)の両方がこれで決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
/// ものだけを、画像に対応するモデルへ送る(legacy/backend.md 4節手順2)。古い画像まで
/// 毎ターン送ると、1枚ごとにコンテキストを大きく使い続けるため。その他は中身を送らない。
pub fn delivery(kind: AttachmentKind, image_input: bool, in_latest_message: bool) -> Delivery {
    match kind {
        AttachmentKind::Text => Delivery::Content,
        AttachmentKind::Image if image_input && in_latest_message => Delivery::Image,
        AttachmentKind::Image | AttachmentKind::Other => Delivery::NameOnly,
    }
}

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
    pub fn stage(&self, name: String, bytes: Vec<u8>) -> Result<StageOutcome> {
        self.staged.stage(name, bytes)
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
