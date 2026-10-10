//! 設定・登録のコマンド。規則と秘密情報の保存は`scitl_core::settings`にあり、ここは引数を
//! 渡して1つ呼ぶだけ。設定を変えるコマンドは、設定画面が受け取るのと同じ変更後の設定全体を出す。

use std::sync::Arc;

use clap::Subcommand;
use secrecy::SecretString;
use serde::de::DeserializeOwned;

use scitl_cli::terminal::print_json;
use scitl_cli::Session;
use scitl_core::blocking;
use scitl_core::config::ApiFormat;
use scitl_core::error::Result as CoreResult;
use scitl_core::i18n::Language;
use scitl_core::settings::{
    GeneralUpdate, HeaderInput, NewMcpEndpoint, NewProvider, Settings, SettingsView,
};

use crate::{load_settings, DebugError};

#[derive(Subcommand)]
pub enum SettingsCommand {
    /// Show the settings as the settings screen receives them: general settings, providers
    /// with their models, and MCP servers. Secrets are not included.
    Show,
    /// Replace the general settings. Values left out go back to their defaults.
    General {
        #[arg(long, value_name = "TEXT")]
        system_prompt: Option<String>,
        #[arg(long, value_name = "TEXT")]
        task_chat_system_prompt: Option<String>,
        #[arg(long, value_name = "TEXT")]
        task_opening_message: Option<String>,
        #[arg(long, value_name = "SECONDS")]
        response_timeout_secs: Option<String>,
    },
    /// Replace the limits on tool calls. Values left out go back to their defaults.
    Tools {
        #[arg(long, value_name = "COUNT")]
        max_rounds_per_turn: Option<String>,
        #[arg(long, value_name = "SECONDS")]
        total_timeout_secs: Option<String>,
    },
    /// Set the display language of the desktop app.
    Language {
        /// Language code, as config.toml spells it.
        #[arg(value_parser = parse_config_value::<Language>)]
        language: Language,
    },
}

#[derive(Subcommand)]
pub enum ProviderCommand {
    /// Register a provider. Its id is in the printed settings.
    Add {
        #[arg(long)]
        name: String,
        /// API dialect, as config.toml spells it.
        #[arg(long, value_name = "FORMAT", value_parser = parse_config_value::<ApiFormat>)]
        api_format: ApiFormat,
        #[arg(long, value_name = "URL")]
        base_url: String,
        /// Environment variable of this process that holds the API key. The key goes to the
        /// OS credential store and is never printed.
        #[arg(long, value_name = "VAR")]
        api_key_env: Option<String>,
        /// Request header NAME, taking its value from this process's variable VAR. The value
        /// goes to the OS credential store. `{session_id}` in the value is replaced with an ID
        /// of the conversation when a turn is sent. Can be repeated.
        #[arg(long, value_name = "NAME=VAR")]
        header: Vec<String>,
    },
    /// Remove a provider and its stored API key and header values.
    Delete { provider_id: String },
    /// Ask the provider which models it offers. Nothing is saved.
    Models { provider_id: String },
}

#[derive(Subcommand)]
pub enum ModelCommand {
    /// Register models of a provider.
    Add {
        provider_id: String,
        #[arg(required = true)]
        models: Vec<String>,
    },
    /// Remove a registered model.
    Remove { provider_id: String, model: String },
    /// Select the model the chat uses.
    Select { provider_id: String, model: String },
}

#[derive(Subcommand)]
pub enum McpCommand {
    /// Register a server reached over streamable HTTP.
    AddHttp {
        /// Request header NAME, taking its value from this process's variable VAR. The value
        /// goes to the OS credential store. Can be repeated.
        #[arg(long, value_name = "NAME=VAR")]
        header: Vec<String>,
        name: String,
        url: String,
    },
    /// Remove a server and its stored secrets.
    Delete { server_id: String },
    /// Let turns use a server.
    Enable { server_id: String },
    /// Keep turns from using a server.
    Disable { server_id: String },
    /// Offer a tool of a server to the model.
    EnableTool { server_id: String, tool: String },
    /// Stop offering a tool of a server to the model.
    DisableTool { server_id: String, tool: String },
    /// Connect to a server and show the tools it offers. Nothing is saved.
    FetchTools { server_id: String },
}

/// 設定ファイルと同じ綴りで値を読む。綴りをここに写さず、設定の型の読み方に任せる。
fn parse_config_value<T: DeserializeOwned>(arg: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(arg.to_string())).map_err(|e| e.to_string())
}

/// 秘密情報を、名前で指されたこのプロセスの環境変数から読む。値を引数で受け取ると
/// プロセスの一覧に残るので、引数には名前だけを書かせる。`option`は名前を受け取った引数。
///
/// 名前の形をしていない引数は、値そのものを書いた取り違えとみなし、引数の中身を出さずに断る
/// (clapに検査させると、エラー文が引数をそのまま端末へ書く)。空の値も断る。名前まで指して
/// 空なのは変数の入れ忘れで、coreは空の鍵を鍵なしとして通してしまう。
fn secret_from_env(option: &'static str, var: &str) -> Result<SecretString, DebugError> {
    let mut chars = var.chars();
    let named = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !named {
        return Err(DebugError::NotAVariableName(option));
    }
    match std::env::var(var) {
        Ok(value) if !value.is_empty() => Ok(SecretString::from(value)),
        _ => Err(DebugError::MissingEnv(var.to_string())),
    }
}

/// `NAME=VAR`の並びを、サーバーへ渡す名前と、環境変数`VAR`から読んだ値の組にする。
fn secrets_from_env(
    option: &'static str,
    bindings: Vec<String>,
) -> Result<Vec<(String, SecretString)>, DebugError> {
    bindings
        .into_iter()
        .map(|binding| match binding.split_once('=') {
            Some((name, var)) if !name.is_empty() => {
                Ok((name.to_string(), secret_from_env(option, var)?))
            }
            _ => Err(DebugError::NotAVariableName(option)),
        })
        .collect()
}

/// 設定を変える操作を1つ呼び、変更後の設定を出す。資格情報ストアとファイルのI/Oを伴うので
/// `blocking::run`で呼ぶ。
async fn change<F>(settings: Arc<Settings>, f: F) -> Result<(), DebugError>
where
    F: FnOnce(&Settings) -> CoreResult<SettingsView> + Send + 'static,
{
    print_json(&blocking::run(move || f(&settings)).await?);
    Ok(())
}

pub async fn run_settings(session: &Session, command: SettingsCommand) -> Result<(), DebugError> {
    let settings = load_settings(session).await?;
    match command {
        SettingsCommand::Show => {
            print_json(&settings.view());
            Ok(())
        }
        SettingsCommand::General {
            system_prompt,
            task_chat_system_prompt,
            task_opening_message,
            response_timeout_secs,
        } => {
            let update = GeneralUpdate {
                system_prompt,
                task_chat_system_prompt,
                task_opening_message,
                // 省いた値は既定値に戻す(空欄と同じ)。
                response_timeout_secs: Some(response_timeout_secs.unwrap_or_default()),
            };
            change(settings, move |s| s.update_general(update)).await
        }
        SettingsCommand::Tools {
            max_rounds_per_turn,
            total_timeout_secs,
        } => {
            change(settings, move |s| {
                // 省いた値は既定値に戻す(空欄と同じ)。
                s.update_tools(
                    Some(max_rounds_per_turn.as_deref().unwrap_or_default()),
                    Some(total_timeout_secs.as_deref().unwrap_or_default()),
                )
            })
            .await
        }
        SettingsCommand::Language { language } => {
            change(settings, move |s| s.update_language(language)).await
        }
    }
}

pub async fn run_provider(session: &Session, command: ProviderCommand) -> Result<(), DebugError> {
    let settings = load_settings(session).await?;
    match command {
        ProviderCommand::Add {
            name,
            api_format,
            base_url,
            api_key_env,
            header,
        } => {
            let new = NewProvider {
                name,
                api_format,
                base_url,
                api_key: api_key_env
                    .as_deref()
                    .map(|var| secret_from_env("--api-key-env", var))
                    .transpose()?,
                headers: HeaderInput::Pairs(secrets_from_env("--header", header)?),
            };
            // 利用者自身が端末で打つので、通信先は確かめない(`cli.md`)。
            change(settings, move |s| s.add_provider(new, |_| true)).await
        }
        ProviderCommand::Delete { provider_id } => {
            change(settings, move |s| s.delete_provider(&provider_id)).await
        }
        ProviderCommand::Models { provider_id } => {
            print_json(&settings.list_provider_models(&provider_id).await?);
            Ok(())
        }
    }
}

pub async fn run_model(session: &Session, command: ModelCommand) -> Result<(), DebugError> {
    let settings = load_settings(session).await?;
    match command {
        ModelCommand::Add {
            provider_id,
            models,
        } => {
            print_json(&settings.add_models(&provider_id, models).await?);
            Ok(())
        }
        ModelCommand::Remove { provider_id, model } => {
            change(settings, move |s| s.remove_model(&provider_id, &model)).await
        }
        ModelCommand::Select { provider_id, model } => {
            change(settings, move |s| {
                s.select_chat_model(&provider_id, &model)?;
                Ok(s.view())
            })
            .await
        }
    }
}

pub async fn run_mcp(session: &Session, command: McpCommand) -> Result<(), DebugError> {
    let settings = load_settings(session).await?;
    match command {
        McpCommand::AddHttp { header, name, url } => {
            let endpoint = NewMcpEndpoint::StreamableHttp {
                url,
                headers: HeaderInput::Pairs(secrets_from_env("--header", header)?),
            };
            // 利用者自身が端末で打つので、通信先は確かめない(`cli.md`)。
            print_json(&settings.add_mcp_server(name, endpoint, |_| true).await?);
            Ok(())
        }
        McpCommand::Delete { server_id } => {
            change(settings, move |s| s.delete_mcp_server(&server_id)).await
        }
        McpCommand::Enable { server_id } => {
            change(settings, move |s| {
                s.set_mcp_server_enabled(&server_id, true)
            })
            .await
        }
        McpCommand::Disable { server_id } => {
            change(settings, move |s| {
                s.set_mcp_server_enabled(&server_id, false)
            })
            .await
        }
        McpCommand::EnableTool { server_id, tool } => {
            change(settings, move |s| {
                s.set_mcp_tool_enabled(&server_id, &tool, true)
            })
            .await
        }
        McpCommand::DisableTool { server_id, tool } => {
            change(settings, move |s| {
                s.set_mcp_tool_enabled(&server_id, &tool, false)
            })
            .await
        }
        McpCommand::FetchTools { server_id } => {
            print_json(&settings.fetch_mcp_tools(&server_id).await?);
            Ok(())
        }
    }
}
