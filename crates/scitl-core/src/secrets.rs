//! `keyring`(OS資格情報ストア)呼び出しの唯一の入口(architecture.md 6節)。
//! 他のどのモジュール(LLMアダプタ・Tauriコマンド・CLI)もここを経由せず`keyring`に
//! 直接触れてはいけない。呼び出し元は不透明な`key_ref`だけを扱い、値は`SecretString`から
//! 出し入れする(平文`String`を経由させない)。
//!
//! プロバイダーのAPIキーもMCPサーバーの秘密情報も、同じこの入口を使う
//! (principles.md 5節「同じ仕組みを使う」)。

use keyring::Entry;
use secrecy::{ExposeSecret, SecretString};

use crate::db::error::CoreError;

/// OS資格情報ストア上でのサービス名。複数アプリと資格情報が混ざらないよう固定する。
const SERVICE: &str = "scitl-task-companion";

fn entry(key_ref: &str) -> Result<Entry, CoreError> {
    Entry::new(SERVICE, key_ref).map_err(|e| CoreError::Secrets(e.to_string()))
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
