//! 端末IDの確かめ(`docs/sync/index.md`「端末ID」)。同期を始めるとき(どちらの役でも)に、今のDBの
//! 端末IDの項目がこのマシンの資格情報ストアにあり、値がDBの持つ値と同じかを見る。同じなら両方の値を
//! 替え、無いか違えば写したDBか戻したDBとみなし、端末IDを作り直す。初めて同期するDBも項目が無いので
//! 作り直すが、それまでの版とカーソルはそのまま使えるので困らない。
//!
//! 項目を読めないときも、作り直した項目を書ければ作り直して続ける(Androidで保存先の鍵が失われ、
//! 古い項目だけが読めなくなった場合)。作り直しは要らなかったときも害が無く、要るときに
//! しないと写したDBを見逃す。書けもしなければ、確かめられないので始めない。
//!
//! DBと資格情報ストアは同時には書けないので、DBを先に書き、項目を書けなければDBを戻す。その間で
//! 切れると値が食い違い、次の同期で作り直す。

use std::sync::Arc;

use scitl_core::db::{self, SharedConnection};
use scitl_core::{blocking, secrets, CoreError};

use crate::SyncError;

/// 端末IDの項目の置き場所。普段は資格情報ストア([`SecretStore`])で、試験では差し替える。
pub trait DeviceRegistry: Send + Sync {
    /// 項目があり、値が`token`と同じか。読めなければ失敗を返す(無いことと分ける)。
    fn is_current(&self, device_id: &str, token: &str) -> Result<bool, CoreError>;
    /// 項目を作るか、値を`token`に替える。
    fn register(&self, device_id: &str, token: &str) -> Result<(), CoreError>;
}

pub struct SecretStore;

impl DeviceRegistry for SecretStore {
    fn is_current(&self, device_id: &str, token: &str) -> Result<bool, CoreError> {
        secrets::sync_device_is_current(device_id, token)
    }

    fn register(&self, device_id: &str, token: &str) -> Result<(), CoreError> {
        secrets::register_sync_device(device_id, token)
    }
}

pub async fn confirm_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    let current = db::with_conn(db.clone(), db::sync::device_token).await?;
    let check = registry.clone();
    match blocking::run(move || check.is_current(&current.device_id, &current.token)).await {
        Ok(true) => replace_token(db, registry, false).await,
        Ok(false) => replace_token(db, registry, true).await,
        Err(unreadable) => replace_token(db, registry, true)
            .await
            .map_err(|_| SyncError::DeviceCheck(unreadable)),
    }
}

/// 端末IDを作り直し、項目を作る。
pub async fn renew_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    replace_token(db, registry, true).await
}

/// DBと項目の値を替え(`renew_id`なら端末IDも作り直し)、端末IDを返す。項目を書けなければDBを元に
/// 戻す(端末IDを作り直したまま戻さないと、同期のたびに作り直すことになる)。
async fn replace_token(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
    renew_id: bool,
) -> Result<String, SyncError> {
    let (before, replaced) = db::with_conn(db.clone(), move |conn| {
        db::sync::replace_device_token(conn, renew_id)
    })
    .await?;
    let written = replaced.clone();
    if let Err(e) =
        blocking::run(move || registry.register(&written.device_id, &written.token)).await
    {
        db::with_conn(db, move |conn| {
            db::sync::restore_device_token(conn, &before, &replaced)
        })
        .await?;
        return Err(SyncError::DeviceCheck(e));
    }
    Ok(replaced.device_id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// 資格情報ストアの代わり。`unreadable`なら読めず、`unwritable`なら書けない。
    #[derive(Default)]
    struct Registry {
        items: Mutex<HashMap<String, String>>,
        unreadable: bool,
        unwritable: bool,
    }

    impl DeviceRegistry for Registry {
        fn is_current(&self, device_id: &str, token: &str) -> Result<bool, CoreError> {
            if self.unreadable {
                return Err(CoreError::Secrets("locked".to_string()));
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(device_id)
                .map(String::as_str)
                == Some(token))
        }

        fn register(&self, device_id: &str, token: &str) -> Result<(), CoreError> {
            if self.unwritable {
                return Err(CoreError::Secrets("locked".to_string()));
            }
            self.items
                .lock()
                .unwrap()
                .insert(device_id.to_string(), token.to_string());
            Ok(())
        }
    }

    impl Registry {
        fn token(&self, device_id: &str) -> Option<String> {
            self.items.lock().unwrap().get(device_id).cloned()
        }
    }

    fn database() -> SharedConnection {
        Arc::new(std::sync::Mutex::new(db::open_in_memory().unwrap()))
    }

    fn device_token(db: &SharedConnection) -> db::sync::DeviceToken {
        db::sync::device_token(&db.lock().unwrap()).unwrap()
    }

    /// 写したDB。確かめに使うのは端末IDと値だけなので、それだけを写す。
    fn copy_of(db: &SharedConnection) -> SharedConnection {
        let state = device_token(db);
        let copy = database();
        copy.lock()
            .unwrap()
            .execute(
                "UPDATE sync_state SET device_id = ?1, device_token = ?2",
                [&state.device_id, &state.token],
            )
            .unwrap();
        copy
    }

    #[tokio::test]
    async fn a_current_device_keeps_its_id_and_gets_a_new_token() {
        let db = database();
        let registry = Arc::new(Registry::default());
        let first = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        let before = device_token(&db);
        assert_eq!(
            confirm_device_id(db.clone(), registry.clone())
                .await
                .unwrap(),
            first
        );
        let after = device_token(&db);
        assert_eq!(after.device_id, before.device_id);
        assert_ne!(after.token, before.token);
        assert_eq!(registry.token(&first), Some(after.token));
    }

    /// 写したDB(と、初めて同期するDB)は、項目が無いので作り直し、新しい項目を作る。
    #[tokio::test]
    async fn an_unregistered_device_gets_a_new_id() {
        let db = database();
        let registry = Arc::new(Registry::default());
        let before = device_token(&db).device_id;
        let renewed = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        assert_ne!(renewed, before);
        assert_eq!(device_token(&db).device_id, renewed);
        assert_eq!(registry.token(&renewed), Some(device_token(&db).token));
        assert_eq!(confirm_device_id(db, registry).await.unwrap(), renewed);
    }

    /// 同じマシンで写したDBと戻したDBは、項目の値が先へ進んでいるので見分けられる。先に同期した
    /// 方が端末IDを保ち、遅れた方が作り直す。
    #[tokio::test]
    async fn a_copy_left_behind_by_a_later_sync_gets_a_new_id() {
        let original = database();
        let registry = Arc::new(Registry::default());
        let id = confirm_device_id(original.clone(), registry.clone())
            .await
            .unwrap();
        let copy = copy_of(&original);
        assert_eq!(
            confirm_device_id(original.clone(), registry.clone())
                .await
                .unwrap(),
            id
        );
        let renewed = confirm_device_id(copy.clone(), registry.clone())
            .await
            .unwrap();
        assert_ne!(renewed, id);
        // 元のDBの項目は残り、元のDBはそのまま続けられる。
        assert_eq!(confirm_device_id(original, registry).await.unwrap(), id);
    }

    /// DBの値を替えたところで切れ、項目が古いまま残ったときは、次の同期で作り直す。
    #[tokio::test]
    async fn a_token_left_half_replaced_gets_a_new_id() {
        let db = database();
        let registry = Arc::new(Registry::default());
        let id = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        db::sync::replace_device_token(&db.lock().unwrap(), false).unwrap();
        assert_ne!(confirm_device_id(db, registry).await.unwrap(), id);
    }

    /// Androidで保存先の鍵が失われた場合。
    #[tokio::test]
    async fn an_unreadable_item_is_replaced_when_a_new_one_can_be_written() {
        let db = database();
        let before = device_token(&db).device_id;
        let registry = Arc::new(Registry {
            unreadable: true,
            ..Default::default()
        });
        let renewed = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        assert_ne!(renewed, before);
        assert_eq!(registry.token(&renewed), Some(device_token(&db).token));
    }

    #[tokio::test]
    async fn a_locked_store_stops_without_renewing() {
        let db = database();
        let before = device_token(&db);
        let registry = Arc::new(Registry {
            unreadable: true,
            unwritable: true,
            ..Default::default()
        });
        assert!(matches!(
            confirm_device_id(db.clone(), registry).await,
            Err(SyncError::DeviceCheck(_))
        ));
        assert_eq!(device_token(&db), before);
    }

    #[tokio::test]
    async fn an_unregistered_device_keeps_its_id_when_no_item_can_be_written() {
        let db = database();
        let before = device_token(&db);
        let registry = Arc::new(Registry {
            unwritable: true,
            ..Default::default()
        });
        assert!(matches!(
            confirm_device_id(db.clone(), registry).await,
            Err(SyncError::DeviceCheck(_))
        ));
        assert_eq!(device_token(&db), before);
    }

    #[tokio::test]
    async fn a_token_that_cannot_be_replaced_stops_without_changing_the_database() {
        let db = database();
        let open = Arc::new(Registry::default());
        confirm_device_id(db.clone(), open.clone()).await.unwrap();
        let before = device_token(&db);
        let read_only = Arc::new(Registry {
            items: Mutex::new(open.items.lock().unwrap().clone()),
            unwritable: true,
            ..Default::default()
        });
        assert!(matches!(
            confirm_device_id(db.clone(), read_only).await,
            Err(SyncError::DeviceCheck(_))
        ));
        assert_eq!(device_token(&db), before);
    }
}
