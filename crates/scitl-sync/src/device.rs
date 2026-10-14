//! 端末IDの確かめ(`docs/sync/index.md` 10節)。同期を始めるとき(どちらの役でも)に、今のDBの
//! 端末IDの項目がこのマシンの資格情報ストアにあるかを見て、無ければ写されたDBとみなし、
//! 端末IDを作り直す。初めて同期するDBも項目が無いので作り直すが、それまでの版とカーソルは
//! そのまま使えるので困らない。
//!
//! 項目を読めないときも、作り直した項目を書ければ作り直して続ける(Androidで保存先の鍵が失われ、
//! 古い項目だけが読めなくなった場合)。作り直しは要らなかったときも害が無く、要るときに
//! しないと写したDBを見逃す。書けもしなければ、確かめられないので始めない。

use std::sync::Arc;

use scitl_core::db::{self, SharedConnection};
use scitl_core::{blocking, secrets, CoreError};

use crate::SyncError;

/// 端末IDの項目の置き場所。普段は資格情報ストア([`SecretStore`])で、試験では差し替える。
pub trait DeviceRegistry: Send + Sync {
    /// 項目があるか。読めなければ失敗を返す(無いことと分ける)。
    fn is_registered(&self, device_id: &str) -> Result<bool, CoreError>;
    fn register(&self, device_id: &str) -> Result<(), CoreError>;
}

pub struct SecretStore;

impl DeviceRegistry for SecretStore {
    fn is_registered(&self, device_id: &str) -> Result<bool, CoreError> {
        secrets::sync_device_registered(device_id)
    }

    fn register(&self, device_id: &str) -> Result<(), CoreError> {
        secrets::register_sync_device(device_id)
    }
}

/// 確かめた端末IDを返す。
pub async fn confirm_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    let current = db::with_conn(db.clone(), db::sync::device_state)
        .await?
        .device_id;
    let id = current.clone();
    let check = registry.clone();
    match blocking::run(move || check.is_registered(&id)).await {
        Ok(true) => Ok(current),
        Ok(false) => renew_device_id(db, registry).await,
        Err(unreadable) => renew_device_id(db, registry)
            .await
            .map_err(|_| SyncError::DeviceCheck(unreadable)),
    }
}

/// 端末IDを作り直し、項目を作る。項目を書けなければ元の端末IDに戻す(書けないまま作り直すと、
/// 同期のたびに作り直すことになる)。
pub async fn renew_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    let (before, renewed) = db::with_conn(db.clone(), |conn| {
        let before = db::sync::device_state(conn)?.device_id;
        Ok((before, db::sync::renew_device_id(conn)?))
    })
    .await?;
    let id = renewed.clone();
    if let Err(e) = blocking::run(move || registry.register(&id)).await {
        db::with_conn(db, move |conn| db::sync::set_device_id(conn, &before)).await?;
        return Err(SyncError::DeviceCheck(e));
    }
    Ok(renewed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;

    /// 資格情報ストアの代わり。`unreadable`なら読めず、`locked`なら書けもしない。
    #[derive(Default)]
    struct Registry {
        items: Mutex<HashSet<String>>,
        unreadable: bool,
        locked: bool,
    }

    impl DeviceRegistry for Registry {
        fn is_registered(&self, device_id: &str) -> Result<bool, CoreError> {
            if self.unreadable || self.locked {
                return Err(CoreError::Secrets("locked".to_string()));
            }
            Ok(self.items.lock().unwrap().contains(device_id))
        }

        fn register(&self, device_id: &str) -> Result<(), CoreError> {
            if self.locked {
                return Err(CoreError::Secrets("locked".to_string()));
            }
            self.items.lock().unwrap().insert(device_id.to_string());
            Ok(())
        }
    }

    fn database() -> SharedConnection {
        Arc::new(std::sync::Mutex::new(db::open_in_memory().unwrap()))
    }

    fn device_id(db: &SharedConnection) -> String {
        db::sync::device_state(&db.lock().unwrap())
            .unwrap()
            .device_id
    }

    #[tokio::test]
    async fn a_registered_device_keeps_its_id() {
        let db = database();
        let registry = Arc::new(Registry::default());
        registry.register(&device_id(&db)).unwrap();
        let before = device_id(&db);
        assert_eq!(
            confirm_device_id(db.clone(), registry).await.unwrap(),
            before
        );
        assert_eq!(device_id(&db), before);
    }

    /// 写したDB(と、初めて同期するDB)は、項目が無いので作り直し、新しい項目を作る。
    #[tokio::test]
    async fn an_unregistered_device_gets_a_new_id() {
        let db = database();
        let registry = Arc::new(Registry::default());
        let before = device_id(&db);
        let renewed = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        assert_ne!(renewed, before);
        assert_eq!(device_id(&db), renewed);
        assert!(registry.is_registered(&renewed).unwrap());
        assert_eq!(confirm_device_id(db, registry).await.unwrap(), renewed);
    }

    /// 古い項目だけが読めない(Androidで保存先の鍵が失われた)ときは、作り直して続ける。
    #[tokio::test]
    async fn an_unreadable_item_is_replaced_when_a_new_one_can_be_written() {
        let db = database();
        let before = device_id(&db);
        let registry = Arc::new(Registry {
            unreadable: true,
            ..Default::default()
        });
        let renewed = confirm_device_id(db.clone(), registry.clone())
            .await
            .unwrap();
        assert_ne!(renewed, before);
        assert!(registry.items.lock().unwrap().contains(&renewed));
    }

    #[tokio::test]
    async fn a_locked_store_stops_without_renewing() {
        let db = database();
        let before = device_id(&db);
        let registry = Arc::new(Registry {
            locked: true,
            ..Default::default()
        });
        assert!(matches!(
            confirm_device_id(db.clone(), registry).await,
            Err(SyncError::DeviceCheck(_))
        ));
        assert_eq!(device_id(&db), before);
    }
}
