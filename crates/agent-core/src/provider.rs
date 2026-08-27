//! AI provider clients: turn a text prompt into a text reply.
//!
//! This module only sends text and gets text back. Phase 5 asks the
//! reply to be MIDI-note JSON, and parses it into a [`crate::MidiClip`],
//! but that logic does not live here, to keep this module reusable for
//! any prompt, not only a MIDI-generation one.
//!
//! Every provider here is a blocking (synchronous) HTTP call, not an
//! `async` one. `agent-plugin` runs `AiProvider::complete()` inside a
//! `nih_plug` background task, on a thread `nih_plug` manages, so there
//! is no need for a separate `async` runtime such as `tokio`. See
//! `TODO.md`, section 5.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Something that can turn a text prompt into a text reply.
pub trait AiProvider {
    fn complete(&self, prompt: &str) -> Result<String, ProviderError>;
}

/// An error from an [`AiProvider`].
#[derive(Debug)]
pub enum ProviderError {
    /// The HTTP request itself could not complete: no connection, a
    /// timeout, a TLS problem, and so on.
    Request(String),
    /// The server answered, but with a non-success status code.
    Api { status: u16, message: String },
    /// The server's response was not shaped the way this provider
    /// expects.
    UnexpectedResponse(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Request(message) => write!(f, "request failed: {message}"),
            ProviderError::Api { status, message } => {
                write!(f, "provider returned status {status}: {message}")
            }
            ProviderError::UnexpectedResponse(message) => {
                write!(f, "unexpected response from provider: {message}")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// Tries to pull a human-readable message out of an error response
/// body. Both the OpenAI and the Anthropic APIs shape their error
/// bodies as `{"error": {"message": "...", ...}}`, so one function
/// covers both. Returns `None` if the body is not shaped this way.
fn error_message_from_body(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        message: String,
    }
    serde_json::from_str::<ErrorBody>(body)
        .ok()
        .map(|body| body.error.message)
}

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("building an HTTP client with the default TLS setup should not fail")
}

/// A client for OpenAI-compatible chat APIs. This one client shape
/// covers OpenAI itself, DeepSeek, OpenRouter, and local servers such
/// as Ollama and LM Studio, because they all accept the same
/// `POST {base_url}/chat/completions` request and response shape.
pub struct OpenAiCompatibleProvider {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatibleProvider {
    /// `base_url` must not have a trailing slash, for example
    /// `https://api.openai.com/v1`, or `http://localhost:11434/v1` for
    /// a local Ollama server.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: build_client(),
        }
    }
}

impl AiProvider for OpenAiCompatibleProvider {
    fn complete(&self, prompt: &str) -> Result<String, ProviderError> {
        #[derive(Deserialize)]
        struct ChatResponse {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: ChatMessage,
        }
        #[derive(Deserialize)]
        struct ChatMessage {
            content: String,
        }

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .map_err(|err| ProviderError::Request(err.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .map_err(|err| ProviderError::Request(err.to_string()))?;

        if !status.is_success() {
            let message = error_message_from_body(&body).unwrap_or_else(|| body.clone());
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&body)
            .map_err(|err| ProviderError::UnexpectedResponse(format!("{err}: {body}")))?;

        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| ProviderError::UnexpectedResponse("no choices in response".to_string()))
    }
}

/// A client for the Anthropic Claude Messages API.
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl AnthropicProvider {
    pub const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com";
    const API_VERSION: &'static str = "2023-06-01";
    const MAX_TOKENS: u32 = 4096;

    /// Uses [`Self::DEFAULT_BASE_URL`]. Use
    /// [`Self::with_base_url`] to point at a different URL, such as a
    /// local proxy.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_base_url(Self::DEFAULT_BASE_URL, api_key, model)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: build_client(),
        }
    }
}

impl AiProvider for AnthropicProvider {
    fn complete(&self, prompt: &str) -> Result<String, ProviderError> {
        #[derive(Deserialize)]
        struct MessagesResponse {
            content: Vec<ContentBlock>,
        }
        #[derive(Deserialize)]
        struct ContentBlock {
            #[serde(rename = "type")]
            kind: String,
            text: Option<String>,
        }

        let url = format!("{}/v1/messages", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", Self::API_VERSION)
            .json(&json!({
                "model": self.model,
                "max_tokens": Self::MAX_TOKENS,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .map_err(|err| ProviderError::Request(err.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .map_err(|err| ProviderError::Request(err.to_string()))?;

        if !status.is_success() {
            let message = error_message_from_body(&body).unwrap_or_else(|| body.clone());
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: MessagesResponse = serde_json::from_str(&body)
            .map_err(|err| ProviderError::UnexpectedResponse(format!("{err}: {body}")))?;

        parsed
            .content
            .into_iter()
            .find(|block| block.kind == "text")
            .and_then(|block| block.text)
            .ok_or_else(|| {
                ProviderError::UnexpectedResponse("no text content in response".to_string())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Starts a minimal HTTP/1.1 server on a random local port, that
    /// replies once with a fixed status line and body, then stops.
    /// Returns the base URL to send a request to. This avoids making
    /// real network calls, or adding an HTTP mocking crate, just to
    /// test how this module parses a response.
    fn mock_server_once(status_line: &'static str, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let addr = listener
            .local_addr()
            .expect("mock server should have an address");

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf); // discard the request
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        format!("http://{addr}")
    }

    #[test]
    fn openai_compatible_extracts_the_reply_text() {
        let base_url = mock_server_once(
            "HTTP/1.1 200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"hello there"}}]}"#.to_string(),
        );
        let provider = OpenAiCompatibleProvider::new(base_url, "test-key", "test-model");

        let reply = provider.complete("say hello").expect("should succeed");
        assert_eq!(reply, "hello there");
    }

    #[test]
    fn openai_compatible_turns_an_error_status_into_api_error() {
        let base_url = mock_server_once(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"invalid API key","type":"invalid_request_error"}}"#.to_string(),
        );
        let provider = OpenAiCompatibleProvider::new(base_url, "bad-key", "test-model");

        let err = provider.complete("say hello").expect_err("should fail");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "invalid API key");
            }
            other => panic!("expected ProviderError::Api, got {other:?}"),
        }
    }

    #[test]
    fn openai_compatible_reports_unexpected_json_shapes_instead_of_panicking() {
        let base_url = mock_server_once("HTTP/1.1 200 OK", r#"{"unexpected":true}"#.to_string());
        let provider = OpenAiCompatibleProvider::new(base_url, "test-key", "test-model");

        let err = provider.complete("say hello").expect_err("should fail");
        assert!(matches!(err, ProviderError::UnexpectedResponse(_)));
    }

    #[test]
    fn anthropic_extracts_the_reply_text() {
        let base_url = mock_server_once(
            "HTTP/1.1 200 OK",
            r#"{"content":[{"type":"text","text":"hello from claude"}]}"#.to_string(),
        );
        let provider =
            AnthropicProvider::with_base_url(base_url, "test-key", "claude-3-5-haiku-20241022");

        let reply = provider.complete("say hello").expect("should succeed");
        assert_eq!(reply, "hello from claude");
    }

    #[test]
    fn anthropic_turns_an_error_status_into_api_error() {
        let base_url = mock_server_once(
            "HTTP/1.1 429 Too Many Requests",
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#
                .to_string(),
        );
        let provider =
            AnthropicProvider::with_base_url(base_url, "test-key", "claude-3-5-haiku-20241022");

        let err = provider.complete("say hello").expect_err("should fail");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "slow down");
            }
            other => panic!("expected ProviderError::Api, got {other:?}"),
        }
    }
}
