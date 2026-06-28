//! LLM provider abstraction and an OpenAI-compatible implementation.
//!
//! The [`LlmProvider`] trait is the seam the enrichment pipeline calls; tests
//! supply fakes and production uses [`OpenAiProvider`], which targets any
//! OpenAI-compatible `/v1` endpoint (including a local `llama.cpp` server). The
//! provider builds the versioned prompt, requests a schema-constrained JSON
//! response, and repairs a malformed first response once before giving up.

pub(crate) mod prompt;

use self::prompt::ChatMessage;
use crate::models::{EnrichmentOutput, SavedPost};
use reqwest::Client;
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;
use url::Url;

const DEFAULT_OPENAI_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const DEFAULT_OPENAI_MODEL: &str = "llama.cpp";
const DEFAULT_OPENAI_TIMEOUT_SECS: u64 = 60;

/// An error raised while enriching a single post via an [`LlmProvider`].
#[derive(Debug, Error)]
pub enum EnrichError {
    /// The HTTP request to the provider failed (network, status, body read).
    #[error("LLM transport failed: {0}")]
    Transport(String),
    /// The endpoint is unreachable or the requested model is not available.
    #[error("LLM endpoint is unavailable: {0}")]
    ModelUnavailable(String),
    /// The response could not be parsed into the expected JSON shape.
    #[error("LLM response parse failed: {0}")]
    Parse(String),
    /// The parsed response failed semantic validation (e.g. out-of-range score).
    #[error("LLM output validation failed: {0}")]
    Validation(String),
}

impl EnrichError {
    /// Whether this failure is worth retrying. Only infrastructure faults
    /// (`Transport`, `ModelUnavailable`) are transient: a later attempt may
    /// succeed once the network blip clears or the endpoint comes back. `Parse`
    /// and `Validation` are deterministic given the same prompt and input, so
    /// retrying them just burns the budget — fail fast instead.
    pub fn is_transient(&self) -> bool {
        // Exhaustive on purpose: a new variant must make a deliberate
        // retryable/terminal decision here rather than defaulting to
        // non-retryable behind a `matches!` wildcard.
        match self {
            Self::Transport(_) | Self::ModelUnavailable(_) => true,
            Self::Parse(_) | Self::Validation(_) => false,
        }
    }
}

/// Result type for provider operations, using [`EnrichError`].
pub type EnrichResult<T> = std::result::Result<T, EnrichError>;
/// The boxed future returned by [`LlmProvider::enrich`].
pub type EnrichFuture<'a> =
    Pin<Box<dyn Future<Output = EnrichResult<EnrichmentResult>> + Send + 'a>>;

/// Abstraction over an LLM backend that enriches a single saved post.
///
/// Implemented by [`OpenAiProvider`] in production and by fakes in tests; the
/// enrichment batch calls only through this trait.
pub trait LlmProvider {
    /// Enrich `post`, returning the raw response and parsed output (or an error).
    fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a>;
}

/// A successful enrichment: the parsed output plus the raw model response.
#[derive(Debug, Clone)]
pub struct EnrichmentResult {
    /// The unparsed response text from the provider, retained for auditing.
    pub raw_response: String,
    /// The validated, normalized enrichment output.
    pub output: EnrichmentOutput,
}

/// Connection settings for an OpenAI-compatible chat-completions endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL of the `/v1` API (trailing slash normalized for joining).
    pub base_url: Url,
    /// Model identifier to request.
    pub model: String,
    /// Optional bearer token; omitted for unauthenticated local servers.
    pub api_key: Option<String>,
}

impl OpenAiConfig {
    /// Build a config from environment variables (`RUSTY_RSS_OPENAI_BASE_URL`,
    /// `RUSTY_RSS_OPENAI_MODEL`, `RUSTY_RSS_OPENAI_API_KEY`), falling back to
    /// local-server defaults. Returns an error if the resulting URL/model is
    /// invalid.
    pub fn from_env() -> EnrichResult<Self> {
        let base_url = std::env::var("RUSTY_RSS_OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.to_string());
        let model = std::env::var("RUSTY_RSS_OPENAI_MODEL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());
        let api_key = std::env::var("RUSTY_RSS_OPENAI_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty());

        Self::new(base_url, model, api_key)
    }

    /// Build a config from explicit values, validating the model is non-empty
    /// and normalizing `base_url` for endpoint joining.
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> EnrichResult<Self> {
        if model.trim().is_empty() {
            return Err(EnrichError::Validation("model is required".to_string()));
        }

        let base_url = normalize_base_url(&base_url)?;

        Ok(Self {
            base_url,
            model,
            api_key,
        })
    }
}

/// An [`LlmProvider`] backed by an OpenAI-compatible chat-completions API.
pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: Client,
}

impl OpenAiProvider {
    /// Create a provider from `config`, building an HTTP client with a default
    /// request timeout.
    pub fn new(config: OpenAiConfig) -> Self {
        Self {
            config,
            client: Client::builder()
                .timeout(Duration::from_secs(DEFAULT_OPENAI_TIMEOUT_SECS))
                .build()
                .expect("reqwest client build should not fail"),
        }
    }

    /// The model identifier this provider requests.
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Verify the endpoint is reachable and the configured model is listed by
    /// `GET /models`, so a batch fails fast with a clear error instead of per
    /// item. Returns [`EnrichError::ModelUnavailable`] otherwise.
    pub async fn preflight(&self) -> EnrichResult<()> {
        let response = self
            .request(self.endpoint("models")?)
            .send()
            .await
            .map_err(|err| EnrichError::ModelUnavailable(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(EnrichError::ModelUnavailable(format!(
                "GET /models returned {status}"
            )));
        }

        let models = response
            .json::<ModelsResponse>()
            .await
            .map_err(|err| EnrichError::ModelUnavailable(err.to_string()))?;

        if models
            .data
            .iter()
            .any(|model| model.id == self.config.model)
        {
            Ok(())
        } else {
            Err(EnrichError::ModelUnavailable(format!(
                "model '{}' not found in /models",
                self.config.model
            )))
        }
    }

    async fn enrich_post(&self, post: &SavedPost) -> EnrichResult<EnrichmentResult> {
        let messages = prompt::build_enrichment_messages(post, prompt::MAX_CONTENT_CHARS);
        let raw_response = self.chat_completion(messages).await?;

        match parse_enrichment_output(&raw_response) {
            Ok(output) => Ok(EnrichmentResult {
                raw_response,
                output,
            }),
            Err(first_err) => {
                let repaired_response = self
                    .chat_completion(prompt::build_repair_messages(post, &raw_response))
                    .await?;
                let output = parse_enrichment_output(&repaired_response).map_err(|second_err| {
                    EnrichError::Parse(format!("{first_err}; repair failed: {second_err}"))
                })?;
                Ok(EnrichmentResult {
                    raw_response: repaired_response,
                    output,
                })
            }
        }
    }

    async fn chat_completion(&self, messages: Vec<ChatMessage>) -> EnrichResult<String> {
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature: 0.0,
            response_format: response_format_schema(),
        };

        let response = self
            .request(self.endpoint("chat/completions")?)
            .json(&request)
            .send()
            .await
            .map_err(|err| EnrichError::Transport(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(EnrichError::Transport(format!(
                "POST /chat/completions returned {status}: {body}"
            )));
        }

        let response = response
            .json::<ChatCompletionResponse>()
            .await
            .map_err(|err| EnrichError::Parse(err.to_string()))?;

        response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| EnrichError::Parse("chat completion returned no content".to_string()))
    }

    fn endpoint(&self, path: &str) -> EnrichResult<Url> {
        self.config
            .base_url
            .join(path)
            .map_err(|err| EnrichError::Validation(format!("invalid OpenAI endpoint: {err}")))
    }

    fn request(&self, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.get(url.clone());
        let request = if let Some(api_key) = &self.config.api_key {
            request.bearer_auth(api_key)
        } else {
            request
        };

        if url.path().ends_with("/models") {
            request
        } else {
            let request = self.client.post(url);
            if let Some(api_key) = &self.config.api_key {
                request.bearer_auth(api_key)
            } else {
                request
            }
        }
    }
}

impl LlmProvider for OpenAiProvider {
    fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
        Box::pin(async move { self.enrich_post(post).await })
    }
}

#[derive(Debug, Serialize, Clone)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    response_format: Value,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
}

fn normalize_base_url(base_url: &str) -> EnrichResult<Url> {
    let mut base_url = Url::parse(base_url)
        .map_err(|err| EnrichError::Validation(format!("invalid OpenAI base URL: {err}")))?;

    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }

    Ok(base_url)
}

fn parse_enrichment_output(raw_response: &str) -> std::result::Result<EnrichmentOutput, String> {
    let output = serde_json::from_str::<EnrichmentOutput>(raw_response.trim())
        .map_err(|err| err.to_string())?;
    output.validate()?;
    Ok(output)
}

fn response_format_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "rusty_rss_enrichment",
            "strict": true,
            "schema": schema_for!(EnrichmentOutput)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::serve_json_responses;

    fn test_post() -> SavedPost {
        let mut post = SavedPost::new(
            "t3_test123".to_string(),
            "Interesting Rust tool".to_string(),
            "https://reddit.com/r/rust/comments/test123/tool/".to_string(),
            "atom".to_string(),
        );
        post.author = Some("rustacean".to_string());
        post.subreddit = Some("rust".to_string());
        post.outbound_url = Some("https://example.com/tool".to_string());
        post.content_markdown = Some("A useful Rust tool for agents.".to_string());
        post
    }

    fn success_body(summary: &str) -> String {
        format!(
            r#"{{"choices":[{{"message":{{"content":"{{\"classification\":\"reference\",\"tags\":[\"rust\",\"agents\"],\"summary\":\"{summary}\",\"joy_value\":0.4,\"work_value\":0.9,\"recommended_action\":\"reference_only\",\"rationale\":\"Useful for later implementation work\",\"confidence\":0.8}}"}}}}]}}"#
        )
    }

    fn provider_for(base_url: String) -> OpenAiProvider {
        let config = OpenAiConfig::new(
            base_url,
            "test-model".to_string(),
            Some("test-key".to_string()),
        )
        .expect("config should build");
        OpenAiProvider::new(config)
    }

    #[tokio::test]
    async fn preflight_succeeds_when_model_exists() {
        let (base_url, requests) = serve_json_responses(vec![
            r#"{"data":[{"id":"test-model"},{"id":"other-model"}]}"#.to_string(),
        ]);
        let provider = provider_for(base_url);

        provider.preflight().await.expect("preflight should pass");

        let request = requests.recv().expect("request should be captured");
        assert!(request.starts_with("GET /v1/models "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
    }

    #[tokio::test]
    async fn preflight_fails_when_model_is_missing() {
        let (base_url, _requests) =
            serve_json_responses(vec![r#"{"data":[{"id":"different-model"}]}"#.to_string()]);
        let provider = provider_for(base_url);

        let err = provider
            .preflight()
            .await
            .expect_err("missing model should fail");

        assert!(err.to_string().contains("test-model"));
    }

    #[tokio::test]
    async fn enrich_posts_with_openai_compatible_chat_completion() {
        let (base_url, requests) = serve_json_responses(vec![success_body("Good reference")]);
        let provider = provider_for(base_url);

        let result = provider
            .enrich(&test_post())
            .await
            .expect("enrichment should succeed");

        assert_eq!(result.output.summary, "Good reference");
        let request = requests.recv().expect("request should be captured");
        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        assert!(request.contains("\"model\":\"test-model\""));
        assert!(request.contains("\"type\":\"json_schema\""));
        assert!(request.contains("Interesting Rust tool"));
    }

    #[tokio::test]
    async fn enrich_repairs_malformed_json_once() {
        let malformed = r#"{"choices":[{"message":{"content":"not json"}}]}"#.to_string();
        let (base_url, requests) = serve_json_responses(vec![malformed, success_body("Repaired")]);
        let provider = provider_for(base_url);

        let result = provider
            .enrich(&test_post())
            .await
            .expect("repair should succeed");

        assert_eq!(result.output.summary, "Repaired");
        let _first_request = requests.recv().expect("first request should be captured");
        let second_request = requests.recv().expect("second request should be captured");
        assert!(second_request.contains("previous response was invalid"));
    }

    /// Serve a single HTTP response with an arbitrary status line and body on a
    /// fresh socket, returning the `/v1` base URL. Lets the LLM error paths
    /// (non-2xx, malformed JSON) be exercised without a real model server.
    fn serve_status(status: &str, body: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        let status = status.to_string();
        let body = body.to_string();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        format!("http://{addr}/v1")
    }

    #[test]
    fn from_env_uses_defaults_then_overrides() {
        // from_env must fall back to the built-in defaults when nothing is set,
        // and honor the env overrides when present. The base URL is normalized
        // with a trailing slash either way. SAFETY: env is mutated then restored
        // within this single test; no other core unit test reads these vars.
        unsafe {
            std::env::remove_var("RUSTY_RSS_OPENAI_BASE_URL");
            std::env::remove_var("RUSTY_RSS_OPENAI_MODEL");
            std::env::remove_var("RUSTY_RSS_OPENAI_API_KEY");
        }
        let defaults = OpenAiConfig::from_env().expect("defaults should build");
        assert_eq!(defaults.base_url.as_str(), "http://127.0.0.1:8080/v1/");
        assert_eq!(defaults.model, "llama.cpp");
        assert!(defaults.api_key.is_none());

        unsafe {
            std::env::set_var("RUSTY_RSS_OPENAI_BASE_URL", "https://api.example.com/v1");
            std::env::set_var("RUSTY_RSS_OPENAI_MODEL", "gpt-test");
            // Whitespace-only keys are filtered out to None.
            std::env::set_var("RUSTY_RSS_OPENAI_API_KEY", "   ");
        }
        let overridden = OpenAiConfig::from_env().expect("override should build");
        assert_eq!(overridden.base_url.as_str(), "https://api.example.com/v1/");
        assert_eq!(overridden.model, "gpt-test");
        assert!(
            overridden.api_key.is_none(),
            "blank api key should be filtered to None"
        );

        unsafe {
            std::env::set_var("RUSTY_RSS_OPENAI_API_KEY", "real-key");
        }
        let with_key = OpenAiConfig::from_env().expect("config should build");
        assert_eq!(with_key.api_key.as_deref(), Some("real-key"));

        unsafe {
            std::env::remove_var("RUSTY_RSS_OPENAI_BASE_URL");
            std::env::remove_var("RUSTY_RSS_OPENAI_MODEL");
            std::env::remove_var("RUSTY_RSS_OPENAI_API_KEY");
        }
    }

    #[test]
    fn new_rejects_empty_model() {
        let err = OpenAiConfig::new(
            "http://127.0.0.1:8080/v1".to_string(),
            "   ".to_string(),
            None,
        )
        .expect_err("blank model should fail validation");
        assert!(matches!(err, EnrichError::Validation(_)), "got: {err:?}");
        assert!(err.to_string().contains("model is required"));
    }

    #[test]
    fn new_rejects_invalid_base_url() {
        let err = OpenAiConfig::new("not a url".to_string(), "test-model".to_string(), None)
            .expect_err("invalid base URL should fail validation");
        assert!(matches!(err, EnrichError::Validation(_)), "got: {err:?}");
        assert!(err.to_string().contains("invalid OpenAI base URL"));
    }

    #[tokio::test]
    async fn chat_completion_non_2xx_is_transport_error() {
        let provider = provider_for(serve_status("500 Internal Server Error", "upstream boom"));

        let err = provider
            .enrich(&test_post())
            .await
            .expect_err("non-2xx should fail");

        assert!(matches!(err, EnrichError::Transport(_)), "got: {err:?}");
        assert!(err.to_string().contains("500"));
        assert!(err.to_string().contains("upstream boom"));
    }

    #[tokio::test]
    async fn chat_completion_unparseable_body_is_parse_error() {
        let provider = provider_for(serve_status("200 OK", "definitely not json"));

        let err = provider
            .enrich(&test_post())
            .await
            .expect_err("unparseable envelope should fail");

        assert!(matches!(err, EnrichError::Parse(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn chat_completion_empty_content_is_parse_error() {
        // A well-formed envelope whose only choice has blank content must be
        // rejected with the dedicated "no content" parse error.
        let provider = provider_for(serve_status(
            "200 OK",
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
        ));

        let err = provider
            .enrich(&test_post())
            .await
            .expect_err("empty content should fail");

        assert!(matches!(err, EnrichError::Parse(_)), "got: {err:?}");
        assert!(err.to_string().contains("no content"), "got: {err}");
    }

    #[tokio::test]
    async fn enrich_returns_parse_error_when_repair_also_fails() {
        // Both the initial completion and the repair attempt return content that
        // is not valid enrichment JSON, so enrich must surface a Parse error that
        // mentions the repair failure rather than retrying forever.
        let envelope = r#"{"choices":[{"message":{"content":"still not valid"}}]}"#.to_string();
        let (base_url, _requests) = serve_json_responses(vec![envelope.clone(), envelope]);
        let provider = provider_for(base_url);

        let err = provider
            .enrich(&test_post())
            .await
            .expect_err("two malformed responses should fail");

        assert!(matches!(err, EnrichError::Parse(_)), "got: {err:?}");
        assert!(err.to_string().contains("repair failed"), "got: {err}");
    }

    #[tokio::test]
    async fn preflight_non_2xx_is_model_unavailable() {
        let provider = provider_for(serve_status("503 Service Unavailable", "down"));

        let err = provider
            .preflight()
            .await
            .expect_err("non-2xx /models should fail");

        assert!(
            matches!(err, EnrichError::ModelUnavailable(_)),
            "got: {err:?}"
        );
        assert!(err.to_string().contains("503"));
    }

    #[tokio::test]
    async fn preflight_unparseable_body_is_model_unavailable() {
        let provider = provider_for(serve_status("200 OK", "not a models list"));

        let err = provider
            .preflight()
            .await
            .expect_err("unparseable /models body should fail");

        assert!(
            matches!(err, EnrichError::ModelUnavailable(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn normalizes_base_url_for_endpoint_joining() {
        let config = OpenAiConfig::new(
            "http://127.0.0.1:8080/v1".to_string(),
            "test-model".to_string(),
            None,
        )
        .expect("config should build");
        let endpoint = OpenAiProvider::new(config)
            .endpoint("chat/completions")
            .expect("endpoint should build");

        assert_eq!(
            endpoint.as_str(),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }
}
