//! 画面の入力を受け付けなかった理由のうち、画面が欄の近くに表示言語で出すもの。理由は種類で
//! 返し、文言は画面が種類から引く(`architecture/i18n.md`)。判定はここ(core)だけで行い、画面は
//! 写さない(`architecture/webview-boundary.md`「画面が持つもの・持たないもの」)。

use std::fmt;

use serde::Serialize;

use crate::error::{CoreError, Result};

/// 数値の欄のどれか。CLIのエラー文に名前を出すのと、画面がどの欄の下に出すかを決めるのに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NumberField {
    ResponseTimeoutSecs,
    MaxRoundsPerTurn,
    TotalTimeoutSecs,
    ContextLength,
}

impl NumberField {
    fn name(self) -> &'static str {
        match self {
            Self::ResponseTimeoutSecs => "response timeout (seconds)",
            Self::MaxRoundsPerTurn => "max rounds per turn",
            Self::TotalTimeoutSecs => "tool timeout (seconds)",
            Self::ContextLength => "context length",
        }
    }
}

/// 受け付けなかった理由の1つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputRejection {
    /// 数値の欄が1以上の整数でない。
    NotPositiveInteger { field: NumberField },
    /// 数値の欄が上限を超える。
    NumberTooLarge {
        field: NumberField,
        #[cfg_attr(test, ts(type = "number"))]
        max: u64,
    },
    /// MCPサーバーの識別子が空。
    McpServerNameRequired,
    /// MCPサーバーの識別子に使えない文字・並びがあるか、長すぎる
    /// (`config::validate_mcp_server_name`)。
    McpServerNameInvalid { max_chars: usize },
    /// 同じ識別子のMCPサーバーが登録済み。
    McpServerNameTaken { name: String },
    /// MCPサーバーのURLが空。
    UrlRequired,
    /// ヘッダーの欄の`line_no`行目(1から数える)が`NAME=VALUE`の形でないか、名前がHTTPの
    /// ヘッダー名として読めない(`NAME: VALUE`と書いた等)。値は秘密情報なので、行の中身は返さない。
    HeaderLineInvalid { line_no: usize },
}

impl fmt::Display for InputRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPositiveInteger { field } => {
                write!(f, "{} must be an integer of 1 or more", field.name())
            }
            Self::NumberTooLarge { field, max } => {
                write!(f, "{} must be at most {max}", field.name())
            }
            Self::McpServerNameRequired => f.write_str("MCP server name must not be empty"),
            Self::McpServerNameInvalid { max_chars } => write!(
                f,
                "MCP server name must be at most {max_chars} letters, digits or single \
                 underscores between them"
            ),
            Self::McpServerNameTaken { name } => {
                write!(f, "MCP server name already registered: {name}")
            }
            Self::UrlRequired => f.write_str("URL must not be empty"),
            Self::HeaderLineInvalid { line_no } => {
                write!(f, "header line {line_no} is not in the NAME=VALUE form")
            }
        }
    }
}

/// 1回の操作で見つかった理由の並び([`CoreError::Rejected`])。空にはしない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejections(pub Vec<InputRejection>);

impl fmt::Display for Rejections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, reason) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{reason}")?;
        }
        Ok(())
    }
}

/// 理由の並び(空でない)を、断る失敗にする。
pub(crate) fn rejected(reasons: Vec<InputRejection>) -> CoreError {
    debug_assert!(!reasons.is_empty());
    CoreError::Rejected(Rejections(reasons))
}

/// 理由が1つでもあれば断る。
pub(crate) fn refuse_if_any(reasons: Vec<InputRejection>) -> Result<()> {
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(rejected(reasons))
    }
}

/// 画面の入力を受け取る操作の結果。受け付けなかった理由([`CoreError::Rejected`])は失敗に
/// せず種類で返し、画面は欄の近くに出す。それ以外の失敗はコマンドの失敗のまま。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FormOutcome<T> {
    Accepted { value: T },
    Rejected { reasons: Vec<InputRejection> },
}

impl<T> FormOutcome<T> {
    pub fn from_result(result: Result<T>) -> Result<Self> {
        match result {
            Ok(value) => Ok(Self::Accepted { value }),
            Err(CoreError::Rejected(Rejections(reasons))) => Ok(Self::Rejected { reasons }),
            Err(e) => Err(e),
        }
    }
}
