//! MCP streamable_http用のHTTPクライアント設定。architecture.md 5節のハードニングを
//! `llm/providers/openai_compat.rs`と同じ強度で適用する(Opusレビュー指摘)。
//!
//! `reqwest::Client`という型そのものはワークスペース既定のreqwest(0.12、LLMアダプタが
//! 使う)とここで使う`mcp-reqwest`(0.13、rmcpが要求するバージョン)とで異なる型のため
//! 共有できない。設定内容の正は`docs/spec/rebuild/architecture.md`5節であり、
//! ここと`openai_compat.rs`の両方がそこを参照する。

use std::time::Duration;

use mcp_reqwest as reqwest;

use crate::db::error::CoreError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// URLの妥当性検証のみ行う(サーバー登録時、実際に接続する前のIPC層からも呼ぶ)。
pub(super) fn validate(url: &str) -> Result<(), CoreError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Mcp(format!("url is not a valid URL: {e}")))?;
    crate::net::validate_external_url(&parsed).map_err(CoreError::Mcp)
}

/// MCPサーバーへのURLを検証してから、ハードニング済み`reqwest::Client`を組み立てる。
pub fn hardened_client(url: &str, request_timeout: Duration) -> Result<reqwest::Client, CoreError> {
    validate(url)?;

    reqwest::Client::builder()
        .no_proxy()
        // architecture.md 5節: クロスホストのリダイレクトは拒否する。MCPサーバーは
        // 正当な理由でリダイレクトを返すことを想定していないため、
        // openai_compat.rsと同様に同一ホスト内も含めて一律拒否する
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        .map_err(|e| CoreError::Mcp(format!("failed to build HTTP client: {e}")))
}

// 以下のテストは、`rmcp`のstreamable_httpトランスポートが内部的に`self.get()`等で
// このクライアントをそのまま使う(`transport/common/reqwest/streamable_http_client.rs`)
// ことを踏まえ、トランスポートの外側からは検証できない「実際にリダイレクトを追わないか」
// 「プロキシ環境変数を無視するか」をクライアント単体に対して確認する
// (Opusレビュー指摘: ドキュメントではなくテストで担保する)。
#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// 1回だけ接続を受け、`response`をそのまま書いて閉じる最小限のHTTPサーバー。
    fn spawn_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn cross_host_redirect_is_not_followed() {
        let url = spawn_once(
            "HTTP/1.1 302 Found\r\nLocation: http://example.invalid/elsewhere\r\nContent-Length: 0\r\n\r\n",
        );
        let client = hardened_client(&url, Duration::from_secs(5)).unwrap();
        // リダイレクトを追っていれば別ホストへの接続を試みて失敗するはずが、
        // ここでは追わずに302がそのまま返ってくることを確認する
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 302);
    }

    #[tokio::test]
    async fn proxy_env_var_is_ignored() {
        let url = spawn_once("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        // このプロセス内の他のテストはenv varを触らないため、並行実行下でも安全
        // (今後env varを操作するテストを足す場合は要注意)。
        // SAFETY: `set_var`/`remove_var`はプロセス全体のグローバル状態を変更するため
        // unsafeとされているが、このテストバイナリ内で環境変数を操作する他のテストは
        // 無く、競合は起きない。
        unsafe { std::env::set_var("http_proxy", "http://127.0.0.1:1") };
        let client = hardened_client(&url, Duration::from_secs(5)).unwrap();
        let result = client.get(&url).send().await;
        unsafe { std::env::remove_var("http_proxy") };
        // プロキシ(存在しない127.0.0.1:1)を経由していれば失敗するはずが、成功する
        assert!(result.is_ok());
    }
}
