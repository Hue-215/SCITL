//! 送信前の添付。選んだ時点で判定と実体の保存を済ませてトークンを返し、送信のときに
//! トークンから取り出す。判定を画面に写さず、選んだ時点で結果(種別・大きさの上限)を
//! 見せられるようにするため。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use serde::Serialize;
use ulid::Ulid;

use super::classify::{classify, LIMITS};
use super::store::AttachmentStore;
use crate::db::attachments::{AttachmentContent, AttachmentKind, NewAttachment};
use crate::db::error::{CoreError, Result};

/// [`Staged::stage`]の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Rejection {
    TooLarge {
        kind: AttachmentKind,
        limit_bytes: u64,
    },
}

/// 送信前の添付の集合。アプリの起動中だけメモリに持つ(実体は置き場所に書いてあり、
/// 送らずに終えても同じ内容をもう一度添付すれば同じ実体を指す)。
#[derive(Default)]
pub(super) struct Staged {
    entries: Mutex<HashMap<String, NewAttachment>>,
}

impl Staged {
    pub(super) fn stage(
        &self,
        store: &AttachmentStore,
        name: String,
        bytes: &[u8],
    ) -> Result<StageOutcome> {
        if name.trim().is_empty() {
            return Err(CoreError::InvalidArgument {
                name: "name".to_string(),
                reason: "attachment name must not be empty".to_string(),
            });
        }
        let classified = classify(bytes);
        let limit_bytes = LIMITS.bytes_for(classified.kind);
        if bytes.len() as u64 > limit_bytes {
            return Ok(StageOutcome::Rejected {
                reason: Rejection::TooLarge {
                    kind: classified.kind,
                    limit_bytes,
                },
            });
        }
        let content = match classified.kind {
            AttachmentKind::Text => AttachmentContent::Text(
                String::from_utf8(bytes.to_vec()).expect("classified as UTF-8 text"),
            ),
            AttachmentKind::Image | AttachmentKind::Other => AttachmentContent::File {
                hash: store.put(bytes)?,
            },
        };
        let size_bytes = i64::try_from(bytes.len()).expect("bounded by the size limit");
        let token = Ulid::new().to_string();
        self.lock().insert(
            token.clone(),
            NewAttachment {
                original_name: name,
                mime_type: classified.mime_type.to_string(),
                kind: classified.kind,
                size_bytes,
                content,
            },
        );
        Ok(StageOutcome::Staged {
            token,
            kind: classified.kind,
            mime_type: classified.mime_type.to_string(),
            size_bytes,
        })
    }

    /// 知らないトークンは何もしない(破棄と送信が行き違っても困らないように)。
    pub(super) fn discard(&self, token: &str) {
        self.lock().remove(token);
    }

    /// 送る添付を、渡された順に写して返す。まだ集合から外さない(発言を書き終えてから
    /// [`Self::remove`]で外す。書けなかったら、画面は同じトークンで送り直せる)。
    pub(super) fn resolve(&self, tokens: &[String]) -> Result<Vec<NewAttachment>> {
        if tokens.len() > LIMITS.per_message {
            return Err(CoreError::Attachment(format!(
                "a message can carry at most {} attachments",
                LIMITS.per_message
            )));
        }
        let mut seen = HashSet::new();
        let entries = self.lock();
        tokens
            .iter()
            .map(|token| {
                if !seen.insert(token.as_str()) {
                    return Err(CoreError::Attachment(
                        "the same attachment was given twice".to_string(),
                    ));
                }
                entries
                    .get(token)
                    .cloned()
                    .ok_or_else(|| CoreError::Attachment("staged attachment not found".to_string()))
            })
            .collect()
    }

    pub(super) fn remove(&self, tokens: &[String]) {
        let mut entries = self.lock();
        for token in tokens {
            entries.remove(token);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, NewAttachment>> {
        self.entries
            .lock()
            .expect("staged attachments mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> AttachmentStore {
        let root = std::env::temp_dir().join(format!("scitl-staging-{}", Ulid::new()));
        AttachmentStore::new(root.join("blobs"), root.join("revealed"))
    }

    fn token_of(outcome: StageOutcome) -> String {
        match outcome {
            StageOutcome::Staged { token, .. } => token,
            other => panic!("expected staged, got {other:?}"),
        }
    }

    #[test]
    fn keeps_text_in_memory_and_resolves_in_the_given_order() {
        let staged = Staged::default();
        let store = store();
        let a = token_of(staged.stage(&store, "a.txt".into(), b"aaa").unwrap());
        let b = token_of(staged.stage(&store, "b.txt".into(), b"bbb").unwrap());

        let resolved = staged.resolve(&[b.clone(), a.clone()]).unwrap();
        assert_eq!(resolved[0].original_name, "b.txt");
        assert_eq!(
            resolved[1].content,
            AttachmentContent::Text("aaa".to_string())
        );
        // 取り出しただけでは外れない。
        assert!(staged.resolve(std::slice::from_ref(&a)).is_ok());

        staged.remove(std::slice::from_ref(&a));
        assert!(staged.resolve(std::slice::from_ref(&a)).is_err());
        staged.discard(&b);
        assert!(staged.resolve(&[b]).is_err());
    }

    #[test]
    fn rejects_files_over_the_limit_of_their_kind_without_staging() {
        let staged = Staged::default();
        let too_long = vec![b'a'; LIMITS.text_bytes as usize + 1];
        assert_eq!(
            staged.stage(&store(), "big.txt".into(), &too_long).unwrap(),
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
        let store = store();
        let a = token_of(staged.stage(&store, "a.txt".into(), b"a").unwrap());
        assert!(staged.resolve(&[a.clone(), a.clone()]).is_err());
        assert!(staged.resolve(&vec![a; LIMITS.per_message + 1]).is_err());
        assert!(staged.stage(&store, "  ".into(), b"a").is_err());
    }
}
