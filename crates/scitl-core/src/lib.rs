pub mod attachments;
pub mod blocking;
pub mod config;
pub mod db;
pub mod i18n;
pub mod in_flight;
pub mod link;
pub mod llm;
pub mod mcp;
pub mod net;
pub mod orchestration;
pub mod secrets;
pub mod settings;
pub mod text;
pub mod tools;

pub use db::error::CoreError;
