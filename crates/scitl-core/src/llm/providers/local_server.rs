//! 手元で動かす推論サーバー(llama.cpp・LM Studio・Ollama)からのモデル能力の検出(能力解決の
//! 「自動検出」の層)。
//!
//! どのサーバーもチャットはOpenAI互換APIで受けるが、能力の問い合わせ方はサーバーごとの
//! 独自APIにしか無い。問い合わせ先は登録済みの`base_url`と同じオリジンの別のパスだけで、
//! 通信先は増やさない。クライアントも`net::hardened_client`を通す。
//!
//! 問い合わせるのは接続先がループバックかプライベートIPのときだけ([`is_detectable`])。
//! クラウドのAPIはこれらの独自APIを持たず、問い合わせても外れるだけで、
//! 利用者の知らないパスへ鍵を送ることになるため。
//!
//! サーバーの種類は応答の形で見分ける。どの問い合わせにも当てはまらなければ「検出できない
//! サーバー」で、能力は既定値の層に任せる。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use url::Url;

use crate::error::CoreError;
use crate::llm::{DetectedCapabilities, LlmError};
use crate::net::{self, ExternalUrl, HostClass};

use super::Credentials;

/// 検出1回の問い合わせごとの上限。手元のサーバーはすぐに答えるので短くてよく、
/// ターンの開始([`crate::settings::Settings::snapshot_for_turn`])を長く待たせないため。
const DETECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 能力を問い合わせてよい接続先か。
pub fn is_detectable(base_url: &str) -> bool {
    Url::parse(base_url).is_ok_and(|url| {
        matches!(
            net::classify_host(&url),
            HostClass::Loopback | HostClass::PrivateLiteral
        )
    })
}

/// `models`の能力をサーバーに問い合わせる。`Ok(None)`は「能力を問い合わせられる
/// サーバーではない」、`Err`は「サーバーに繋がらない、または今は答えられない」。
/// サーバーが知らないモデルは結果に含めない。
pub async fn detect(
    base_url: &str,
    credentials: &Credentials,
    models: &[String],
) -> Result<Option<HashMap<String, DetectedCapabilities>>, CoreError> {
    if !is_detectable(base_url) {
        return Ok(None);
    }
    let base_url = ExternalUrl::parse(base_url).map_err(CoreError::ProviderConfig)?;
    let probe = Probe {
        client: net::hardened_client(&base_url, net::RequestTimeout::Total(DETECT_TIMEOUT))?,
        root: server_root(&base_url),
        credentials,
    };

    // llama.cppは起動時に読み込んだ1モデルだけを出すので、登録名によらず同じ能力になる。
    if let Some(props) = probe.get::<LlamaCppProps>("props").await? {
        let detected = props.detected();
        return Ok(Some(
            models
                .iter()
                .map(|m| (m.clone(), detected.clone()))
                .collect(),
        ));
    }
    if let Some(list) = probe.get::<LmStudioModels>("api/v0/models").await? {
        return Ok(Some(
            list.data
                .into_iter()
                .filter(|m| models.contains(&m.id))
                .map(|m| (m.id.clone(), m.detected()))
                .collect(),
        ));
    }
    if probe.get::<OllamaVersion>("api/version").await?.is_some() {
        let mut found = HashMap::new();
        for model in models {
            let body = serde_json::json!({ "model": model });
            if let Some(show) = probe.post::<OllamaShow>("api/show", &body).await? {
                found.insert(model.clone(), show.detected());
            }
        }
        return Ok(Some(found));
    }
    Ok(None)
}

/// OpenAI互換APIは`/v1`の下にあり、独自APIはその1つ上にある。`base_url`が`/v1`で
/// 終わらなければそのまま使う(リバースプロキシで前置きのパスが付いていても崩さない)。
fn server_root(base_url: &ExternalUrl) -> Url {
    let mut url = base_url.as_url().clone();
    let path = url.path().trim_end_matches('/');
    let root = path.strip_suffix("/v1").unwrap_or(path);
    url.set_path(&format!("{root}/"));
    url
}

struct Probe<'a> {
    client: reqwest::Client,
    root: Url,
    credentials: &'a Credentials,
}

impl Probe<'_> {
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, CoreError> {
        self.send(self.client.get(self.endpoint(path)?)).await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<Option<T>, CoreError> {
        self.send(self.client.post(self.endpoint(path)?).json(body))
            .await
    }

    fn endpoint(&self, path: &str) -> Result<Url, CoreError> {
        self.root
            .join(path)
            .map_err(|e| CoreError::ProviderConfig(format!("failed to build endpoint: {e}")))
    }

    /// その経路が無いと答えた、または期待した形でなければ「このサーバーではない」として
    /// `None`。繋がらない、またはそれ以外の失敗(読み込み中の503、認証の401等)は`Err`。
    /// 一時的な失敗を「このサーバーではない」と取り違えると、検出できないサーバーとして
    /// 覚えられ、問い合わせ直されない(`settings`)。
    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<Option<T>, CoreError> {
        let response =
            super::send_with_key(request, self.credentials, super::KeyHeader::Bearer, None).await?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            return Ok(None);
        }
        let secrets = self.credentials.secrets();
        let response = super::reject_failure(response, secrets, |status, body| {
            LlmError::from_status(status, body, secrets)
        })
        .await?;
        let body = super::read_body(response, super::MAX_RESPONSE_BYTES, secrets).await?;
        Ok(serde_json::from_slice(&body).ok())
    }
}

/// llama.cpp(llama-server)の`GET /props`。
#[derive(Deserialize)]
struct LlamaCppProps {
    default_generation_settings: LlamaCppGenerationSettings,
    #[serde(default)]
    modalities: Option<LlamaCppModalities>,
    #[serde(default)]
    chat_template_caps: Option<LlamaCppTemplateCaps>,
}

#[derive(Deserialize)]
struct LlamaCppGenerationSettings {
    /// 1リクエストに使えるコンテキスト長(並列数で分けた後の値)。
    n_ctx: Option<u32>,
}

#[derive(Deserialize)]
struct LlamaCppModalities {
    vision: Option<bool>,
}

#[derive(Deserialize)]
struct LlamaCppTemplateCaps {
    supports_tool_calls: Option<bool>,
}

impl LlamaCppProps {
    fn detected(&self) -> DetectedCapabilities {
        DetectedCapabilities {
            image: self.modalities.as_ref().and_then(|m| m.vision),
            tools: self
                .chat_template_caps
                .as_ref()
                .and_then(|c| c.supports_tool_calls),
            // 思考するかどうかはチャットテンプレートとモデル次第で、サーバーは教えない。
            thinking: None,
            context_length: self.default_generation_settings.n_ctx,
        }
    }
}

/// LM Studioの`GET /api/v0/models`。
#[derive(Deserialize)]
struct LmStudioModels {
    data: Vec<LmStudioModel>,
}

#[derive(Deserialize)]
struct LmStudioModel {
    id: String,
    /// `llm`・`vlm`・`embeddings`。
    #[serde(rename = "type")]
    kind: String,
    /// 読み込み済みのときだけある。`max_context_length`は学習時の長さで、読み込み時に
    /// 確保する長さとは違うので使わない。
    #[serde(default)]
    loaded_context_length: Option<u32>,
    #[serde(default)]
    capabilities: Vec<String>,
}

impl LmStudioModel {
    fn detected(&self) -> DetectedCapabilities {
        DetectedCapabilities {
            image: Some(self.kind == "vlm"),
            // `tool_use`はツール呼び出しに向けて学習したモデルにだけ付く。付いていない
            // モデルでもLM Studioはプロンプト経由でツールを渡せるので、無いことからは
            // 非対応と決めない。
            tools: self
                .capabilities
                .iter()
                .any(|c| c == "tool_use")
                .then_some(true),
            thinking: None,
            context_length: self.loaded_context_length,
        }
    }
}

/// Ollamaの`GET /api/version`。Ollamaかどうかを見分けるためだけに使う。
#[derive(Deserialize)]
struct OllamaVersion {
    #[allow(dead_code)]
    version: String,
}

/// Ollamaの`POST /api/show`。
#[derive(Deserialize)]
struct OllamaShow {
    /// 古い版には無い。
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    /// Modelfileの`PARAMETER`を1行ずつ`名前 値`で並べた文字列。
    #[serde(default)]
    parameters: Option<String>,
}

impl OllamaShow {
    fn detected(&self) -> DetectedCapabilities {
        let has = |name: &str| {
            self.capabilities
                .as_ref()
                .map(|caps| caps.iter().any(|c| c == name))
        };
        DetectedCapabilities {
            image: has("vision"),
            tools: has("tools"),
            thinking: has("thinking"),
            // `model_info`の`*.context_length`は学習時の長さで、OpenAI互換APIからの
            // 呼び出しはサーバーの既定の長さで動く。モデルに`num_ctx`が書かれている
            // ときだけ、その長さが使われると分かる。
            context_length: self.num_ctx(),
        }
    }

    fn num_ctx(&self) -> Option<u32> {
        self.parameters.as_deref()?.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == "num_ctx")
                .then(|| parts.next()?.parse().ok())
                .flatten()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;
    use secrecy::SecretString;

    /// 最小限のサーバーが返すもの。
    enum Reply {
        Json(&'static str),
        /// 本文の無い応答の状態行(`404 Not Found`等)。
        Status(&'static str),
    }

    /// 要求の1行目(`GET /props HTTP/1.1`等)から応答を決める、最小限のHTTPサーバー。
    /// 接続ごとに1要求だけ受けて閉じる。受けた要求の1行目を返す。1行目は応答を書く前に
    /// 渡す(書いたあとだと、クライアントが検出を終えて受けた要求を数えるのに間に合わない)。
    fn spawn_server(route: fn(&str) -> Reply) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = request.lines().next().unwrap_or_default().to_string();
                let reply = route(&line);
                let _ = tx.send(line);
                let response = match reply {
                    Reply::Json(body) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    Reply::Status(status) => format!(
                        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}/v1"), rx)
    }

    fn no_key() -> Credentials {
        Credentials::key_only(SecretString::from(String::new()))
    }

    #[tokio::test]
    async fn detects_through_ollama_endpoints() {
        let (base_url, requests) = spawn_server(|line| match line {
            l if l.starts_with("GET /api/version ") => Reply::Json(r#"{"version": "0.9.0"}"#),
            l if l.starts_with("POST /api/show ") => Reply::Json(
                r#"{"parameters": "num_ctx 8192", "capabilities": ["completion", "vision"]}"#,
            ),
            _ => Reply::Status("404 Not Found"),
        });

        let found = detect(&base_url, &no_key(), &["gemma3:4b".to_string()])
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            found["gemma3:4b"],
            DetectedCapabilities {
                image: Some(true),
                tools: Some(false),
                thinking: Some(false),
                context_length: Some(8192),
            }
        );
        // `/v1`の1つ上に問い合わせ、それより外(別のオリジン)には出ていない。
        let paths: Vec<String> = requests.try_iter().collect();
        assert_eq!(
            paths,
            vec![
                "GET /props HTTP/1.1",
                "GET /api/v0/models HTTP/1.1",
                "GET /api/version HTTP/1.1",
                "POST /api/show HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn unknown_servers_are_not_detectable() {
        // 独自APIの経路が無いことの答え方はサーバーによって違う。
        for status in [
            |_: &str| Reply::Status("404 Not Found"),
            |_: &str| Reply::Status("405 Method Not Allowed"),
        ] {
            let (base_url, _) = spawn_server(status);
            let found = detect(&base_url, &no_key(), &["m".to_string()])
                .await
                .unwrap();
            assert!(found.is_none());
        }
    }

    /// 読み込み中・認証の失敗は「このサーバーではない」と区別する(理由は`Probe::send`)。
    #[tokio::test]
    async fn servers_that_cannot_answer_now_are_errors() {
        for route in [
            |_: &str| Reply::Status("503 Service Unavailable"),
            |_: &str| Reply::Status("401 Unauthorized"),
        ] {
            let (base_url, _) = spawn_server(route);
            let result = detect(&base_url, &no_key(), &["m".to_string()]).await;
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn unreachable_servers_are_errors() {
        let (addr, _port) = crate::net::refused_addr();
        let result = detect(&format!("http://{addr}/v1"), &no_key(), &["m".to_string()]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn does_not_contact_remote_hosts() {
        // 検出しない接続先には、繋ぎに行く前に`None`を返す(繋げば失敗して`Err`になる)。
        let found = detect("https://192.0.2.1/v1", &no_key(), &["m".to_string()])
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn detects_only_loopback_and_private_hosts() {
        assert!(is_detectable("http://localhost:11434/v1"));
        assert!(is_detectable("http://127.0.0.1:8080"));
        assert!(is_detectable("http://192.168.1.10:1234/v1"));
        assert!(!is_detectable("https://api.openai.com/v1"));
        assert!(!is_detectable("not a url"));
    }

    #[test]
    fn server_root_strips_only_a_trailing_v1() {
        let root = |u| server_root(&ExternalUrl::parse(u).unwrap()).to_string();
        assert_eq!(root("http://localhost:11434/v1"), "http://localhost:11434/");
        assert_eq!(
            root("http://localhost:11434/v1/"),
            "http://localhost:11434/"
        );
        assert_eq!(root("http://localhost:8080"), "http://localhost:8080/");
        assert_eq!(root("http://10.0.0.2/ollama/v1"), "http://10.0.0.2/ollama/");
        assert_eq!(root("http://10.0.0.2/api"), "http://10.0.0.2/api/");
    }

    #[test]
    fn reads_llama_cpp_props() {
        let props: LlamaCppProps = serde_json::from_str(
            r#"{
                "default_generation_settings": {"n_ctx": 8192, "params": {}},
                "total_slots": 1,
                "modalities": {"vision": true, "audio": false},
                "chat_template_caps": {"supports_tools": true, "supports_tool_calls": true}
            }"#,
        )
        .unwrap();
        assert_eq!(
            props.detected(),
            DetectedCapabilities {
                image: Some(true),
                tools: Some(true),
                thinking: None,
                context_length: Some(8192),
            }
        );
    }

    #[test]
    fn older_llama_cpp_reports_only_context_length() {
        let props: LlamaCppProps =
            serde_json::from_str(r#"{"default_generation_settings": {"n_ctx": 4096}}"#).unwrap();
        assert_eq!(
            props.detected(),
            DetectedCapabilities {
                context_length: Some(4096),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reads_lm_studio_models() {
        let list: LmStudioModels = serde_json::from_str(
            r#"{"object": "list", "data": [
                {"id": "qwen2-vl-7b-instruct", "object": "model", "type": "vlm",
                 "state": "loaded", "max_context_length": 32768,
                 "loaded_context_length": 4096, "capabilities": ["tool_use"]},
                {"id": "llama-3.2-1b", "object": "model", "type": "llm",
                 "state": "not-loaded", "max_context_length": 131072}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            list.data[0].detected(),
            DetectedCapabilities {
                image: Some(true),
                tools: Some(true),
                thinking: None,
                context_length: Some(4096),
            }
        );
        assert_eq!(
            list.data[1].detected(),
            DetectedCapabilities {
                image: Some(false),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reads_ollama_show() {
        let show: OllamaShow = serde_json::from_str(
            r#"{
                "parameters": "stop                           \"<|im_end|>\"\nnum_ctx                        16384",
                "model_info": {"qwen3.context_length": 40960},
                "capabilities": ["completion", "tools", "thinking"]
            }"#,
        )
        .unwrap();
        assert_eq!(
            show.detected(),
            DetectedCapabilities {
                image: Some(false),
                tools: Some(true),
                thinking: Some(true),
                context_length: Some(16384),
            }
        );
    }

    #[test]
    fn older_ollama_without_capabilities_leaves_flags_undetected() {
        let show: OllamaShow = serde_json::from_str(r#"{"modelfile": "FROM x"}"#).unwrap();
        assert_eq!(show.detected(), DetectedCapabilities::default());
    }

    #[test]
    fn does_not_mistake_other_json_for_a_known_server() {
        // OpenAI互換の`/v1/models`の形は、どのサーバーの独自APIとも読めない。
        let body = r#"{"object": "list", "data": [{"id": "m", "object": "model"}]}"#;
        assert!(serde_json::from_str::<LlamaCppProps>(body).is_err());
        assert!(serde_json::from_str::<LmStudioModels>(body).is_err());
        assert!(serde_json::from_str::<OllamaVersion>(body).is_err());
    }
}
