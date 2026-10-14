//! 端末IDの確かめ(`docs/sync/index.md` 10節)。同期を始めるとき(どちらの役でも)に、今のDBの
//! 端末IDの項目がこのマシンの資格情報ストアにあるかを見て、無ければ写されたDBとみなし、
//! 端末IDを作り直す。初めて同期するDBも項目が無いので作り直すが、それまでの版とカーソルは
//! そのまま使えるので困らない。

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

/// 確かめた端末IDを返す。資格情報ストアを読めなければ、確かめられないので作り直しもせずに
/// 失敗にする(同期を始めない)。
pub async fn confirm_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    let current = db::with_conn(db.clone(), db::sync::device_state)
        .await?
        .device_id;
    let id = current.clone();
    let check = registry.clone();
    let registered = blocking::run(move || check.is_registered(&id))
        .await
        .map_err(SyncError::DeviceCheck)?;
    if registered {
        Ok(current)
    } else {
        renew_device_id(db, registry).await
    }
}

/// 端末IDを作り直し、項目を作る。項目を作れずに終わっても、次に同期を始めるときにもう一度
/// 作り直すだけなので困らない。
pub async fn renew_device_id(
    db: SharedConnection,
    registry: Arc<dyn DeviceRegistry>,
) -> Result<String, SyncError> {
    let renewed = db::with_conn(db, db::sync::renew_device_id).await?;
    let id = renewed.clone();
    blocking::run(move || registry.register(&id))
        .await
        .map_err(SyncError::DeviceCheck)?;
    Ok(renewed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;

    /// 資格情報ストアの代わり。`broken`なら読めない。
    #[derive(Default)]
    struct Registry {
        items: Mutex<HashSet<String>>,
        broken: bool,
    }

    impl DeviceRegistry for Registry {
        fn is_registered(&self, device_id: &str) -> Result<bool, CoreError> {
            if self.broken {
                return Err(CoreError::Secrets("locked".to_string()));
            }
            Ok(self.items.lock().unwrap().contains(device_id))
        }

        fn register(&self, device_id: &str) -> Result<(), CoreError> {
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

    #[tokio::test]
    async fn an_unreadable_store_stops_without_renewing() {
        let db = database();
        let before = device_id(&db);
        let registry = Arc::new(Registry {
            broken: true,
            ..Default::default()
        });
        assert!(matches!(
            confirm_device_id(db.clone(), registry).await,
            Err(SyncError::DeviceCheck(_))
        ));
        assert_eq!(device_id(&db), before);
    }
}
