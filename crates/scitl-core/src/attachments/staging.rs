//! 送信前の添付。選んだ時点で判定を済ませてトークンを返し、送信のときにトークンから
//! 取り出す。判定を画面に写さず、選んだ時点で結果(種別・大きさの上限)を見せられるように
//! するため。
//!
//! 中身は送信までメモリにだけ持ち、実体の置き場所には送信のときに書く。選んだ時点で書くと、
//! 取り消した・送らずに終えた添付の実体が、どの行からも指されないまま残り続けるため。
//!
//! 画像は預かる時点で正規化し([`normalize_image`])、以降は正規化したものだけを扱う。
//! 大きさ・MIME・内容のハッシュも正規化した後のもの。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use ulid::Ulid;

use super::classify::{classify, Classified, LIMITS};
use super::normalize::normalize_image;
use super::store::AttachmentStore;
use crate::db::attachments::{AttachmentContent, AttachmentKind, NewAttachment};
use crate::error::{CoreError, Result};

/// [`Staged::stage`]の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StageOutcome {
    Staged {
        token: String,
        kind: AttachmentKind,
        mime_type: String,
        size_bytes: i64,
    },
    /// 受け付けなかった。画面は理由ごとの文言を出す。
    Rejected {
        #[serde(flatten)]
        reason: Rejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Rejection {
    TooLarge {
        kind: AttachmentKind,
        limit_bytes: u64,
    },
    /// 窓に落としたもの・選んだものがファイルでない(フォルダ等)。
    NotAFile,
    /// 送っていない添付が、1つの発言に付けられる数に達している。
    TooMany { limit: usize },
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    classified: Classified,
    bytes: Arc<[u8]>,
}

/// 送信のために預かりから取り出した添付。発言を書けなかったら[`Staged::restore`]で戻す。
#[derive(Debug, Clone)]
pub(crate) struct Taken(Vec<(String, Entry)>);

impl Taken {
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// 実体を置き場所に書き、行として書く形にする。テキストは本文を行に持つので書かない。
    pub(crate) fn store(&self, store: &AttachmentStore) -> Result<Vec<NewAttachment>> {
        self.0
            .iter()
            .map(|(_, entry)| {
                let content = match entry.classified.kind {
                    AttachmentKind::Text => AttachmentContent::Text(
                        String::from_utf8(entry.bytes.to_vec()).expect("classified as UTF-8 text"),
                    ),
                    AttachmentKind::Image | AttachmentKind::Other => AttachmentContent::File {
                        hash: store.put(&entry.bytes)?,
                    },
                };
                Ok(NewAttachment {
                    original_name: entry.name.clone(),
                    mime_type: entry.classified.mime_type.to_string(),
                    kind: entry.classified.kind,
                    size_bytes: size_of(&entry.bytes),
                    content,
                })
            })
            .collect()
    }
}

/// 送信前の添付の集合。アプリの起動中だけメモリに持つ。
///
/// 預かる数は、1つの発言に付けられる数([`LIMITS`]の`per_message`)までにし、達したら新しく
/// 預けようとしたものを断る(古いものを黙って捨てない)。入力欄は1つなので、預かりの数は入力欄に
/// 並ぶ添付の数と一致する(取り消し・送信に失敗して戻した添付も、預かりの出し入れで合う)。
/// 送信までメモリに持つので、乗っ取られた画面から際限なく預けさせない(合計量も、この数と
/// 種別ごとの上限の積で抑えられる)。受け取ったファイルは、読む前に席を取る([`Self::reserve`])。
/// 読んでいる途中のものも数に入れ、並べて受け取っても上限を超える分は読まない。
///
/// 例外は送信に失敗して戻した添付([`Self::restore`])で、その間に足した添付と合わせて上限を超え
/// うる(戻す分を捨てると利用者の添付を失うので、超えたまま戻す。送るときに断られ、利用者が減らす)。
#[derive(Default)]
pub(super) struct Staged {
    entries: Mutex<HashMap<String, Entry>>,
    /// 席を取って読んでいる途中の数。増やすのは`entries`のロックの中だけ。
    reserved: AtomicUsize,
}

/// 受け取ったファイルを読む間の席([`Staged::reserve`])。預けるか、落とせば返る。
pub(super) struct Seat<'a>(&'a Staged);

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        self.0.reserved.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Staged {
    /// 席を取る。預かりと読んでいる途中の数が上限に達していれば`None`で、そのファイルは読まない。
    pub(super) fn reserve(&self) -> Option<Seat<'_>> {
        let entries = self.lock();
        if entries.len() + self.reserved.load(Ordering::Acquire) >= LIMITS.per_message {
            return None;
        }
        self.reserved.fetch_add(1, Ordering::AcqRel);
        Some(Seat(self))
    }

    pub(super) fn stage(&self, name: String, bytes: Vec<u8>) -> Result<StageOutcome> {
        self.stage_into(name, bytes, None)
    }

    /// 席を取って読んだものを預ける。
    pub(super) fn stage_seated(
        &self,
        name: String,
        bytes: Vec<u8>,
        seat: Seat<'_>,
    ) -> Result<StageOutcome> {
        self.stage_into(name, bytes, Some(seat))
    }

    fn stage_into(
        &self,
        name: String,
        bytes: Vec<u8>,
        seat: Option<Seat<'_>>,
    ) -> Result<StageOutcome> {
        if name.trim().is_empty() {
            return Err(CoreError::InvalidArgument {
                name: "name".to_string(),
                reason: "attachment name must not be empty".to_string(),
            });
        }
        let classified = classify(&bytes);
        let limit_bytes = LIMITS.bytes_for(classified.kind);
        if bytes.len() as u64 > limit_bytes {
            return Ok(StageOutcome::Rejected {
                reason: Rejection::TooLarge {
                    kind: classified.kind,
                    limit_bytes,
                },
            });
        }
        let (classified, bytes) = normalized(classified, bytes);
        let size_bytes = size_of(&bytes);
        let token = Ulid::new().to_string();
        let mut entries = self.lock();
        // 席を持っていれば、その席の分は数に入っているので除いて数える。
        let others = self.reserved.load(Ordering::Acquire) - usize::from(seat.is_some());
        if entries.len() + others >= LIMITS.per_message {
            return Ok(too_many());
        }
        entries.insert(
            token.clone(),
            Entry {
                name,
                classified,
                bytes: bytes.into(),
            },
        );
        // 預かりに入れたので、席は返す(ロックの中で返し、数が一時的にも二重にならないように)。
        drop(seat);
        drop(entries);
        Ok(StageOutcome::Staged {
            token,
            kind: classified.kind,
            mime_type: classified.mime_type.to_string(),
            size_bytes,
        })
    }

    /// 預かりを空にする。画面が読み込み直されると、入力欄の添付は消えるのに預かりは残り、
    /// 数の上限に達したまま添付できなくなるので、画面は起動のたびに呼ぶ。
    pub(super) fn clear(&self) {
        self.lock().clear();
    }

    /// 知らないトークンは何もしない(破棄と送信が行き違っても困らないように)。
    pub(super) fn discard(&self, token: &str) {
        self.lock().remove(token);
    }

    /// 送る添付を、渡された順に預かりから取り出す。1つでも取り出せなければ何も取り出さない。
    /// 取り出しと外すことを1回のロックで行い、同じトークンが2つの発言に使われないようにする。
    pub(super) fn take(&self, tokens: &[String]) -> Result<Taken> {
        if tokens.len() > LIMITS.per_message {
            return Err(CoreError::Attachment(format!(
                "a message can carry at most {} attachments",
                LIMITS.per_message
            )));
        }
        let mut seen = HashSet::new();
        if !tokens.iter().all(|t| seen.insert(t.as_str())) {
            return Err(CoreError::Attachment(
                "the same attachment was given twice".to_string(),
            ));
        }
        let mut entries = self.lock();
        if !tokens.iter().all(|t| entries.contains_key(t)) {
            return Err(CoreError::Attachment(
                "staged attachment not found".to_string(),
            ));
        }
        Ok(Taken(
            tokens
                .iter()
                .map(|t| (t.clone(), entries.remove(t).expect("checked above")))
                .collect(),
        ))
    }

    /// 取り出した添付を戻す。画面は同じトークンで送り直せる。
    pub(super) fn restore(&self, taken: Taken) {
        self.lock().extend(taken.0);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .expect("staged attachments mutex poisoned")
    }
}

/// 画像なら正規化したものに置き換える。デコードできない画像は拒まず「その他」として預かる
/// (その他は中身をデコードしないので、壊れた画像を置いておいても害が無い)。
fn normalized(classified: Classified, bytes: Vec<u8>) -> (Classified, Vec<u8>) {
    if classified.kind != AttachmentKind::Image {
        return (classified, bytes);
    }
    match normalize_image(&bytes) {
        Some(image) => (
            Classified {
                kind: AttachmentKind::Image,
                mime_type: image.mime_type,
            },
            image.bytes,
        ),
        None => (
            Classified {
                kind: AttachmentKind::Other,
                mime_type: classified.mime_type,
            },
            bytes,
        ),
    }
}

pub(super) fn too_many() -> StageOutcome {
    StageOutcome::Rejected {
        reason: Rejection::TooMany {
            limit: LIMITS.per_message,
        },
    }
}

fn size_of(bytes: &[u8]) -> i64 {
    i64::try_from(bytes.len()).expect("bounded by the size limit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::TempStore;

    fn token_of(outcome: StageOutcome) -> String {
        match outcome {
            StageOutcome::Staged { token, .. } => token,
            other => panic!("expected staged, got {other:?}"),
        }
    }

    #[test]
    fn takes_in_the_given_order_and_only_once() {
        let staged = Staged::default();
        let a = token_of(staged.stage("a.txt".into(), b"aaa".to_vec()).unwrap());
        let b = token_of(staged.stage("b.txt".into(), b"bbb".to_vec()).unwrap());

        let taken = staged.take(&[b.clone(), a.clone()]).unwrap();
        let names: Vec<&str> = taken.0.iter().map(|(_, e)| e.name.as_str()).collect();
        assert_eq!(names, ["b.txt", "a.txt"]);
        assert!(staged.take(std::slice::from_ref(&a)).is_err());

        staged.restore(taken);
        assert!(staged.take(std::slice::from_ref(&a)).is_ok());
        staged.discard(&b);
        assert!(staged.take(&[b]).is_err());
    }

    #[test]
    fn refuses_new_attachments_once_a_message_is_full_and_keeps_the_staged_ones() {
        let staged = Staged::default();
        let tokens: Vec<String> = (0..LIMITS.per_message)
            .map(|i| token_of(staged.stage(format!("{i}.txt"), b"a".to_vec()).unwrap()))
            .collect();
        assert!(staged.reserve().is_none());
        assert_eq!(
            staged.stage("over.txt".into(), b"a".to_vec()).unwrap(),
            too_many()
        );
        assert!(staged.take(&tokens).is_ok());

        // 取り消した分だけ、また預けられる。
        let a = token_of(staged.stage("a.txt".into(), b"a".to_vec()).unwrap());
        staged.discard(&a);
        assert!(staged.reserve().is_some());
        staged.stage("b.txt".into(), b"a".to_vec()).unwrap();
        staged.clear();
        assert_eq!(staged.lock().len(), 0);
    }

    /// 読んでいる途中の席も数に入れ、席を持って預けるときは自分の席を除いて数える。
    #[test]
    fn seats_count_toward_the_limit_until_they_are_used_or_dropped() {
        let staged = Staged::default();
        let seats: Vec<Seat<'_>> = (0..LIMITS.per_message)
            .map(|_| staged.reserve().unwrap())
            .collect();
        assert!(staged.reserve().is_none());
        assert_eq!(
            staged.stage("x.txt".into(), b"a".to_vec()).unwrap(),
            too_many()
        );
        let mut seats = seats.into_iter();
        let first = seats.next().unwrap();
        assert!(matches!(
            staged
                .stage_seated("a.txt".into(), b"a".to_vec(), first)
                .unwrap(),
            StageOutcome::Staged { .. }
        ));
        assert!(staged.reserve().is_none());
        drop(seats);
        assert_eq!(staged.reserved.load(Ordering::Acquire), 0);
        assert!(staged.reserve().is_some());
    }

    #[test]
    fn takes_nothing_when_any_token_is_unknown() {
        let staged = Staged::default();
        let a = token_of(staged.stage("a.txt".into(), b"a".to_vec()).unwrap());
        assert!(staged.take(&[a.clone(), "missing".to_string()]).is_err());
        assert!(staged.take(&[a]).is_ok());
    }

    #[test]
    fn stores_only_files_and_keeps_text_in_the_row() {
        let temp = TempStore::new();
        let store = &temp.store;
        let staged = Staged::default();
        let text = token_of(staged.stage("a.txt".into(), b"aaa".to_vec()).unwrap());
        let taken = staged.take(&[text]).unwrap();

        let rows = taken.store(store).unwrap();
        assert_eq!(rows[0].content, AttachmentContent::Text("aaa".to_string()));
        assert!(
            !temp.root().join("blobs").exists(),
            "text must not touch the store"
        );

        let image = token_of(staged.stage("p.png".into(), png(4, 4)).unwrap());
        let rows = staged.take(&[image]).unwrap().store(store).unwrap();
        assert!(matches!(&rows[0].content, AttachmentContent::File { hash } if hash.len() == 64));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(width, height)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn stages_images_as_normalized() {
        let staged = Staged::default();
        let outcome = staged.stage("big.png".into(), png(3000, 100)).unwrap();
        let StageOutcome::Staged {
            token,
            kind,
            mime_type,
            size_bytes,
        } = outcome
        else {
            panic!("expected staged, got {outcome:?}");
        };
        assert_eq!(
            (kind, mime_type.as_str()),
            (AttachmentKind::Image, "image/png")
        );
        let taken = staged.take(&[token]).unwrap();
        let bytes = &taken.0[0].1.bytes;
        assert_eq!(size_bytes, bytes.len() as i64);
        assert_eq!(image::load_from_memory(bytes).unwrap().width(), 1568);
    }

    #[test]
    fn stages_undecodable_images_as_other() {
        let staged = Staged::default();
        let broken = b"\x89PNG\r\n\x1a\nbody".to_vec();
        let outcome = staged.stage("broken.png".into(), broken.clone()).unwrap();
        let StageOutcome::Staged {
            token,
            kind,
            mime_type,
            ..
        } = outcome
        else {
            panic!("expected staged, got {outcome:?}");
        };
        assert_eq!(
            (kind, mime_type.as_str()),
            (AttachmentKind::Other, "image/png")
        );
        let taken = staged.take(&[token]).unwrap();
        assert_eq!(&*taken.0[0].1.bytes, broken.as_slice());
    }

    #[test]
    fn rejects_files_over_the_limit_of_their_kind_without_staging() {
        let staged = Staged::default();
        let too_long = vec![b'a'; LIMITS.text_bytes as usize + 1];
        assert_eq!(
            staged.stage("big.txt".into(), too_long).unwrap(),
            StageOutcome::Rejected {
                reason: Rejection::TooLarge {
                    kind: AttachmentKind::Text,
                    limit_bytes: LIMITS.text_bytes
                }
            }
        );
        assert!(staged.lock().is_empty());
    }

    #[test]
    fn refuses_duplicates_too_many_and_empty_names() {
        let staged = Staged::default();
        let a = token_of(staged.stage("a.txt".into(), b"a".to_vec()).unwrap());
        assert!(staged.take(&[a.clone(), a.clone()]).is_err());
        assert!(staged
            .take(&vec![a.clone(); LIMITS.per_message + 1])
            .is_err());
        // 断った取り出しでは外れない。
        assert!(staged.take(&[a]).is_ok());
        assert!(staged.stage("  ".into(), b"a".to_vec()).is_err());
    }
}
