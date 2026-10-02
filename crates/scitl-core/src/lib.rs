// 標準エラーへは`diagnostics`からだけ書く。
#![deny(clippy::print_stderr)]

pub mod attachments;
pub mod blocking;
pub mod config;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod export;
mod files;
pub mod i18n;
pub mod in_flight;
pub mod link;
pub mod llm;
pub mod mcp;
pub mod net;
pub mod orchestration;
pub mod paths;
pub mod secrets;
pub mod settings;
pub mod text;
pub mod tools;

pub use error::CoreError;

/// アプリの識別子。Tauriの`identifier`(`scitl-tauri/tauri.conf.json`)と揃える。データディレクトリの
/// 名前もこれで決まる。設定ファイルはRustの定数を参照できないので、scitl-tauriのテストが照合する。
pub const APP_IDENTIFIER: &str = "net.niigo.scitl";

/// アプリの名前。Tauriの`productName`と揃える(照合は`APP_IDENTIFIER`と同じ)。外部ツールサーバーへの
/// 名乗りに使う。
pub const PRODUCT_NAME: &str = "SCITL Task Companion";
