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

#[derive(Debug, Error)]
pub enum EnrichError {
    #[error("LLM transport failed: {0}")]
    Transport(String),
    #[error("LLM endpoint is unavailable: {0}")]
    ModelUnavailable(String),
    #[error("LLM response parse failed: {0}")]
    Parse(String),
    #[error("LLM output validation failed: {0}")]
    Validation(String),
}

pub type EnrichResult<T> = std::result::Result<T, EnrichError>;
pub type EnrichFuture<'a> =
    Pin<Box<dyn Future<Output = EnrichResult<EnrichmentResult>> + Send + 'a>>;

pub trait LlmProvider {
    fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a>;
}

#[derive(Debug, Clone)]
pub struct EnrichmentResult {
    pub raw_response: String,
    pub output: EnrichmentOutput,
}

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub base_url: Url,
    pub model: String,
    pub api_key: Option<String>,
}

impl OpenAiConfig {
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

pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Self {
        Self {
            config,
            client: Client::builder()
                .timeout(Duration::from_secs(DEFAULT_OPENAI_TIMEOUT_SECS))
                .build()
                .expect("reqwest client build should not fail"),
        }
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

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
