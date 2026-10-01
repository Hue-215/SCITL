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
/// 内部識別子を含むので、件数だけの文に置き換える。
fn store_error(e: keyring_core::Error) -> CoreError {
    match e {
        keyring_core::Error::Ambiguous(items) => CoreError::Secrets(format!(
            "entry is matched by {} credentials in the secret store",
            items.len()
        )),
        e => CoreError::Secrets(e.to_string()),
    }
}

fn entry(key_ref: &str) -> Result<Entry, CoreError> {
    ensure_store()?;
    Entry::new(SERVICE, key_ref).map_err(store_error)
}

/// OSの保存先を、最初に使うときに組み立てて既定にする。組み立てに失敗しても覚えておかず、
/// 次に使うときに組み立て直す(保存先のサービスが後から起動した・ロックが解除された場合に、
/// 再起動せずに直るように)。
fn ensure_store() -> Result<(), CoreError> {
    static SETTING_UP: Mutex<()> = Mutex::new(());
    let _guard = SETTING_UP
        .lock()
        .expect("secret store setup mutex poisoned");
    if keyring_core::get_default_store().is_some() {
        return Ok(());
    }
    let store = os_store().map_err(store_error)?;
    keyring_core::set_default_store(store);
    Ok(())
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
    entry(key_ref)?
        .set_password(secret.expose_secret())
        .map_err(store_error)
}

/// `key_ref`の指す秘密情報を読み出す。未登録の場合はエラーにする
/// (呼び出し元が「キーが空」と「キーが未設定」を区別できるように、値を持たない
/// `Option`ではなく明示的なエラーで返す)。
pub fn load(key_ref: &str) -> Result<SecretString, CoreError> {
    let password = entry(key_ref)?.get_password().map_err(store_error)?;
    Ok(SecretString::from(password))
}

/// `key_ref`の指す秘密情報を削除する。プロバイダー設定を削除する際に呼ぶ。
pub fn delete(key_ref: &str) -> Result<(), CoreError> {
    entry(key_ref)?.delete_credential().map_err(store_error)
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
}
