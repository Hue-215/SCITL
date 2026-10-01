//! OS資格情報ストア呼び出しの唯一の入口。他のどのモジュール(LLMアダプタ・Tauriコマンド・CLI)
//! も、ここを経由せず`keyring_core`に直接触れてはいけない。呼び出し元は不透明な`key_ref`
//! だけを扱い、値は`SecretString`から出し入れする(平文`String`を経由させない)。
//!
//! プロバイダーのAPIキーもMCPサーバーの秘密情報も、同じこの入口を使う。

use std::sync::{Arc, Mutex};

use keyring_core::api::CredentialStore;
use keyring_core::Entry;
use secrecy::{ExposeSecret, SecretString};

use crate::error::CoreError;

/// OS資格情報ストア上でのサービス名。複数アプリと資格情報が混ざらないよう固定する。
const SERVICE: &str = "scitl-task-companion";

/// 保存先のエラーを、画面に出してよい文に変える。`Ambiguous`の表示文だけは保存先の
/// 内部識別子を含むので、件数だけの文に置き換える。`NoEntry`は、設定が指す秘密情報が
/// アプリの外で消された場合に読み出しで起きる。一時的な失敗と違い登録し直すしかないので、
/// そう分かる文にする。
fn store_error(e: keyring_core::Error) -> CoreError {
    match e {
        keyring_core::Error::Ambiguous(items) => CoreError::Secrets(format!(
            "entry is matched by {} credentials in the secret store",
            items.len()
        )),
        keyring_core::Error::NoEntry => CoreError::Secrets(
            "the secret is not in the secret store (it may have been removed outside the app)"
                .to_string(),
        ),
        e => CoreError::Secrets(e.to_string()),
    }
}

/// `key_ref`の項目に`op`を行う。保存先そのものの失敗(`PlatformFailure`)なら、その保存先を
/// 捨てて新しく組み立て、1回だけやり直す([`retry_with_new_store`])。
fn with_entry<T>(
    key_ref: &str,
    op: impl Fn(&Entry) -> keyring_core::Result<T>,
) -> Result<T, CoreError> {
    retry_with_new_store(default_store, discard_store, |store| {
        op(&store.build(SERVICE, key_ref, None)?)
    })
}

/// `acquire`で得た保存先で`op`を行い、`PlatformFailure`なら`discard`してから1回だけ
/// やり直す。
///
/// Linuxの保存先は、組み立てたときに張ったSecret Serviceのセッション1本を使い続ける。
/// セッションの暗号鍵がサービス側と食い違った・サービスが再起動した場合は、そのセッションでの
/// 操作がすべて失敗し続けるので、同じ保存先で繰り返しても直らない。ロック中・承認の拒否
/// (`NoStorageAccess`)はやり直さない(承認を2回求めないため)。
fn retry_with_new_store<S, T>(
    acquire: impl Fn() -> Result<S, CoreError>,
    discard: impl FnOnce(&S),
    op: impl Fn(&S) -> keyring_core::Result<T>,
) -> Result<T, CoreError> {
    let store = acquire()?;
    match op(&store) {
        Err(keyring_core::Error::PlatformFailure(e)) => {
            crate::diagnostics::report(format_args!(
                "secret store operation failed, retrying with a new connection: {e}"
            ));
            discard(&store);
            op(&acquire()?).map_err(store_error)
        }
        result => result.map_err(store_error),
    }
}

/// 保存先の組み立てと破棄を直列にする。
static SETTING_UP: Mutex<()> = Mutex::new(());

/// OSの保存先を、最初に使うときに組み立てて既定にする。組み立てに失敗しても覚えておかず、
/// 次に使うときに組み立て直す(保存先のサービスが後から起動した・ロックが解除された場合に、
/// 再起動せずに直るように)。
fn default_store() -> Result<Arc<CredentialStore>, CoreError> {
    let _guard = SETTING_UP
        .lock()
        .expect("secret store setup mutex poisoned");
    if let Some(store) = keyring_core::get_default_store() {
        return Ok(store);
    }
    let store = os_store().map_err(store_error)?;
    keyring_core::set_default_store(Arc::clone(&store));
    Ok(store)
}

/// 失敗した保存先を既定から外し、次に使うときに組み立て直させる。既定が既に別の保存先に
/// 替わっていれば何もしない(同じ保存先で同時に失敗した別のスレッドが組み立て直したものを
/// 捨てないため)。使用中の`Entry`は、外した保存先のセッションのまま最後まで動く。
fn discard_store(failed: &Arc<CredentialStore>) {
    let _guard = SETTING_UP
        .lock()
        .expect("secret store setup mutex poisoned");
    if keyring_core::get_default_store().is_some_and(|current| Arc::ptr_eq(&current, failed)) {
        keyring_core::unset_default_store();
    }
}

/// Linux: freedesktopのSecret Service(D-Bus経由。組み立てる時点で接続する)。
#[cfg(target_os = "linux")]
fn os_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(dbus_secret_service_keyring_store::Store::new()?)
}

/// Windows: 資格情報マネージャー。
#[cfg(target_os = "windows")]
fn os_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

/// 対応していないOSでは、保存先が無いことをエラーとして返す(黙って仮の保存先に落とさない)。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn os_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(keyring_core::Error::NotSupportedByStore(
        "no OS credential store is supported on this platform".to_string(),
    ))
}

/// `key_ref`の指す秘密情報を保存する(既存があれば上書き)。
pub fn store(key_ref: &str, secret: &SecretString) -> Result<(), CoreError> {
    with_entry(key_ref, |entry| entry.set_password(secret.expose_secret()))
}

/// `key_ref`の指す秘密情報を読み出す。未登録の場合はエラーにする
/// (呼び出し元が「キーが空」と「キーが未設定」を区別できるように、値を持たない
/// `Option`ではなく明示的なエラーで返す)。
pub fn load(key_ref: &str) -> Result<SecretString, CoreError> {
    with_entry(key_ref, |entry| entry.get_password()).map(SecretString::from)
}

/// `key_ref`の指す秘密情報を削除する。プロバイダー設定を削除する際に呼ぶ。既に無ければ
/// 成功にする(やり直したとき、1回目で消えていることがあるため)。
pub fn delete(key_ref: &str) -> Result<(), CoreError> {
    with_entry(key_ref, |entry| match entry.delete_credential() {
        Err(keyring_core::Error::NoEntry) => Ok(()),
        result => result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::mock;

    /// モックの資格情報はDebug表示に`key_ref`を含むので、それが置き換え後の文に残らないことを
    /// 確かめる。既定の保存先は差し替えず、モックを直接包んだ`Entry`で`Ambiguous`を作る。
    #[test]
    fn ambiguous_error_does_not_show_key_ref() {
        let key_ref = "mcp:01JTESTKEYREF";
        let cred = mock::Cred {
            specifiers: (SERVICE.to_string(), key_ref.to_string()),
            inner: Default::default(),
        };
        let items = vec![Entry::new_with_credential(Arc::new(cred))];
        let raw = keyring_core::Error::Ambiguous(items);
        assert!(raw.to_string().contains(key_ref));

        let CoreError::Secrets(message) = store_error(raw) else {
            panic!("expected CoreError::Secrets");
        };
        assert!(!message.contains(key_ref));
    }

    /// 登録し直すしかない失敗だと分かる文にする。
    #[test]
    fn no_entry_error_says_the_secret_is_missing() {
        let CoreError::Secrets(message) = store_error(keyring_core::Error::NoEntry) else {
            panic!("expected CoreError::Secrets");
        };
        assert!(message.contains("not in the secret store"));
    }

    fn platform_failure() -> keyring_core::Error {
        keyring_core::Error::PlatformFailure("broken session".into())
    }

    /// 保存先を番号で表す。`acquire`は呼ばれるたびに新しい番号の保存先を返す。
    fn numbered_stores() -> impl Fn() -> Result<u32, CoreError> {
        let next = std::cell::Cell::new(0);
        move || {
            let n = next.get();
            next.set(n + 1);
            Ok(n)
        }
    }

    #[test]
    fn a_platform_failure_is_retried_once_on_a_new_store() {
        let discarded = std::cell::Cell::new(None);
        let result = retry_with_new_store(
            numbered_stores(),
            |store| discarded.set(Some(*store)),
            |store| {
                if *store == 0 {
                    Err(platform_failure())
                } else {
                    Ok(*store)
                }
            },
        );
        assert_eq!(result.unwrap(), 1);
        assert_eq!(discarded.get(), Some(0));
    }

    #[test]
    fn a_platform_failure_is_not_retried_twice() {
        let tries = std::cell::Cell::new(0);
        let result: Result<(), _> = retry_with_new_store(
            numbered_stores(),
            |_| {},
            |_| {
                tries.set(tries.get() + 1);
                Err(platform_failure())
            },
        );
        assert!(matches!(result, Err(CoreError::Secrets(_))));
        assert_eq!(tries.get(), 2);
    }

    /// ロック中・承認の拒否は、承認を2回求めないようやり直さない。
    #[test]
    fn no_storage_access_is_not_retried() {
        let tries = std::cell::Cell::new(0);
        let result: Result<(), _> = retry_with_new_store(
            numbered_stores(),
            |_| panic!("the store must not be discarded"),
            |_| {
                tries.set(tries.get() + 1);
                Err(keyring_core::Error::NoStorageAccess("locked".into()))
            },
        );
        assert!(result.is_err());
        assert_eq!(tries.get(), 1);
    }
}
