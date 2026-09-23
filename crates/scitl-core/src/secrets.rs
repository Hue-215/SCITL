//! OS資格情報ストア呼び出しの唯一の入口(architecture.md 6節)。他のどのモジュール
//! (LLMアダプタ・Tauriコマンド・CLI)もここを経由せず`keyring_core`に直接触れてはいけない。
//! 呼び出し元は不透明な`key_ref`だけを扱い、値は`SecretString`から出し入れする
//! (平文`String`を経由させない)。
//!
//! プロバイダーのAPIキーもMCPサーバーの秘密情報も、同じこの入口を使う
//! (principles.md 5節「同じ仕組みを使う」)。

use std::sync::{Arc, Mutex};

use keyring_core::api::CredentialStore;
use keyring_core::Entry;
use secrecy::{ExposeSecret, SecretString};

use crate::db::error::CoreError;

/// OS資格情報ストア上でのサービス名。複数アプリと資格情報が混ざらないよう固定する。
const SERVICE: &str = "scitl-task-companion";

fn entry(key_ref: &str) -> Result<Entry, CoreError> {
    ensure_store()?;
    Entry::new(SERVICE, key_ref).map_err(|e| CoreError::Secrets(e.to_string()))
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
    let store = os_store().map_err(|e| CoreError::Secrets(e.to_string()))?;
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
        .map_err(|e| CoreError::Secrets(e.to_string()))
}

/// `key_ref`の指す秘密情報を読み出す。未登録の場合はエラーにする
/// (呼び出し元が「キーが空」と「キーが未設定」を区別できるように、値を持たない
/// `Option`ではなく明示的なエラーで返す)。
pub fn load(key_ref: &str) -> Result<SecretString, CoreError> {
    let password = entry(key_ref)?
        .get_password()
        .map_err(|e| CoreError::Secrets(e.to_string()))?;
    Ok(SecretString::from(password))
}

/// `key_ref`の指す秘密情報を削除する。プロバイダー設定を削除する際に呼ぶ。
pub fn delete(key_ref: &str) -> Result<(), CoreError> {
    entry(key_ref)?
        .delete_credential()
        .map_err(|e| CoreError::Secrets(e.to_string()))
}
