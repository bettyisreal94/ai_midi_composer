//! AI provider clients: turn a text prompt into a text reply.
//!
//! This module only sends text and gets text back. Phase 5 asks the
//! reply to be MIDI-note JSON, and parses it into a [`crate::MidiClip`],
//! but that logic does not live here, to keep this module reusable for
//! any prompt, not only a MIDI-generation one.
//!
//! Every provider here is a blocking (synchronous) HTTP call, not an
//! `async` one. `composer-plugin` runs `AiProvider::complete()` inside a
//! `nih_plug` background task, on a thread `nih_plug` manages, so there
//! is no need for a separate `async` runtime such as `tokio`. See
//! `TODO.md`, section 5.
//!
//! `system_prompt` and `user_prompt` are sent through each API's own
//! system-instruction field (OpenAI-compatible: a `system` role
//! message; Anthropic: the top-level `system` field), not concatenated
//! into one user message. Providers can weigh a system instruction
//! differently from a user one, so keeping them separate matches how
//! these APIs are meant to be used.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

/// A provider call gives up after this long. `composer-plugin` runs every
/// call on `nih_plug`'s shared background worker, so a slow or hung
/// call can hold that worker for up to this long; keep this well under
/// a minute so a stuck request cannot make the plugin feel frozen for
/// too long. Whether plugin unload itself waits for an in-flight call
/// to finish, or time out, is host and `nih_plug` behavior this
/// project has not tested against a real host; see `TODO.md`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A provider reply larger than this is rejected outright, before it
/// is even parsed as JSON. A JSON object describing a few dozen MIDI
/// notes is a few kilobytes at most; this is generous headroom above
/// that, not a realistic expectation.
const MAX_RESPONSE_BYTES: usize = 1_000_000;

/// How long an error message shown to the user, or embedded in a retry
/// prompt, may quote from a raw, unparsed response body. Longer than
/// this is truncated, so a provider that returns a large or malformed
/// body cannot flood the editor or a retry prompt with it.
const MAX_QUOTED_BODY_CHARS: usize = 300;

/// Something that can turn a prompt into a text reply.
pub trait AiProvider {
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, ProviderError>;
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

/// Shortens `text` to at most [`MAX_QUOTED_BODY_CHARS`] characters, so
/// a raw response body embedded in an error message, or a retry
/// prompt, cannot grow without bound.
fn truncate_for_display(text: &str) -> String {
    if text.chars().count() <= MAX_QUOTED_BODY_CHARS {
        text.to_string()
    } else {
        let head: String = text.chars().take(MAX_QUOTED_BODY_CHARS).collect();
        format!("{head}… (truncated)")
    }
}

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
        .map(|body| truncate_for_display(&body.error.message))
}

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("building an HTTP client with the default TLS setup should not fail")
}

/// Describes a failed [`reqwest::blocking::Client::send`] call, walking
/// its `source()` chain, not just its own `Display` text.
///
/// A maintainer found this necessary after seeing an unhelpful error
/// like "error sending request for url (...)" while debugging a local
/// Ollama connection: `reqwest::Error`'s own `Display` only describes
/// the *kind* of failure (building the request, sending it, decoding
/// the response, ...), not *why* it failed. The actual reason (for
/// example "tcp connect error: Connection refused", or a DNS failure,
/// or a timeout) lives one or more levels deeper, in
/// `std::error::Error::source()`, which `Display` does not include by
/// itself. Joining the whole chain gives the user something they can
/// actually act on.
fn describe_request_error(err: reqwest::Error) -> String {
    let mut message = err.to_string();
    let mut cause = std::error::Error::source(&err);
    while let Some(source) = cause {
        message.push_str(": ");
        message.push_str(&source.to_string());
        cause = source.source();
    }
    message
}

/// Joins `base` and `path` with exactly one slash between them,
/// regardless of whether `base` already ends with one. Without this, a
/// base URL with a trailing slash (a very easy mistake to paste in)
/// silently produces a double slash in the request path.
fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Reads `response`'s body, rejecting it outright if it is larger than
/// [`MAX_RESPONSE_BYTES`], instead of buffering an unbounded amount of
/// memory for a misbehaving or hostile server.
fn read_body_bounded(response: reqwest::blocking::Response) -> Result<String, ProviderError> {
    let mut buf = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|err| ProviderError::Request(err.to_string()))?;
    if buf.len() > MAX_RESPONSE_BYTES {
        return Err(ProviderError::UnexpectedResponse(format!(
            "the response was larger than the {MAX_RESPONSE_BYTES}-byte limit"
        )));
    }
    String::from_utf8(buf).map_err(|err| {
        ProviderError::UnexpectedResponse(format!("the response was not valid UTF-8: {err}"))
    })
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
    const MAX_TOKENS: u32 = 4096;

    /// For example `https://api.openai.com/v1`, or
    /// `http://localhost:11434/v1` for a local Ollama server. A
    /// trailing slash is fine; it is removed before use.
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
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, ProviderError> {
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
            /// `None` for a reply this project cannot use as text: a
            /// tool call, a refusal, or another non-text response
            /// shape some OpenAI-compatible servers can return here.
            content: Option<String>,
        }

        let url = join_url(&self.base_url, "chat/completions");
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "max_tokens": Self::MAX_TOKENS,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
            }))
            .send()
            .map_err(|err| ProviderError::Request(describe_request_error(err)))?;

        let status = response.status();
        let body = read_body_bounded(response)?;

        if !status.is_success() {
            let message =
                error_message_from_body(&body).unwrap_or_else(|| truncate_for_display(&body));
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&body).map_err(|err| {
            ProviderError::UnexpectedResponse(format!("{err}: {}", truncate_for_display(&body)))
        })?;

        let choice = parsed.choices.into_iter().next().ok_or_else(|| {
            ProviderError::UnexpectedResponse("no choices in response".to_string())
        })?;

        choice.message.content.ok_or_else(|| {
            ProviderError::UnexpectedResponse(
                "the model's reply had no text content (it may have been a tool call, a \
                 refusal, or another non-text response)"
                    .to_string(),
            )
        })
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

    /// A trailing slash on `base_url` is fine; it is removed before
    /// use.
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
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, ProviderError> {
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

        let url = join_url(&self.base_url, "v1/messages");
        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", Self::API_VERSION)
            .json(&json!({
                "model": self.model,
                "max_tokens": Self::MAX_TOKENS,
                "system": system_prompt,
                "messages": [{"role": "user", "content": user_prompt}],
            }))
            .send()
            .map_err(|err| ProviderError::Request(describe_request_error(err)))?;

        let status = response.status();
        let body = read_body_bounded(response)?;

        if !status.is_success() {
            let message =
                error_message_from_body(&body).unwrap_or_else(|| truncate_for_display(&body));
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: MessagesResponse = serde_json::from_str(&body).map_err(|err| {
            ProviderError::UnexpectedResponse(format!("{err}: {}", truncate_for_display(&body)))
        })?;

        // A reply can have more than one text block. Earlier versions
        // of this client only returned the first one, silently
        // dropping the rest.
        let text_blocks: Vec<String> = parsed
            .content
            .into_iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text)
            .collect();

        if text_blocks.is_empty() {
            return Err(ProviderError::UnexpectedResponse(
                "no text content in response".to_string(),
            ));
        }
        Ok(text_blocks.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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

        let reply = provider
            .complete("you are a helpful assistant", "say hello")
            .expect("should succeed");
        assert_eq!(reply, "hello there");
    }

    #[test]
    fn openai_compatible_accepts_a_trailing_slash_on_the_base_url() {
        let base_url = mock_server_once(
            "HTTP/1.1 200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"hello there"}}]}"#.to_string(),
        );
        let provider =
            OpenAiCompatibleProvider::new(format!("{base_url}/"), "test-key", "test-model");

        let reply = provider
            .complete("system", "say hello")
            .expect("should succeed");
        assert_eq!(reply, "hello there");
    }

    #[test]
    fn openai_compatible_turns_an_error_status_into_api_error() {
        let base_url = mock_server_once(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"invalid API key","type":"invalid_request_error"}}"#.to_string(),
        );
        let provider = OpenAiCompatibleProvider::new(base_url, "bad-key", "test-model");

        let err = provider
            .complete("system", "say hello")
            .expect_err("should fail");
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

        let err = provider
            .complete("system", "say hello")
            .expect_err("should fail");
        assert!(matches!(err, ProviderError::UnexpectedResponse(_)));
    }

    #[test]
    fn openai_compatible_reports_a_null_content_reply_clearly() {
        // A tool-call or refusal reply from a real OpenAI-compatible
        // server can have `"content": null`.
        let base_url = mock_server_once(
            "HTTP/1.1 200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":null}}]}"#.to_string(),
        );
        let provider = OpenAiCompatibleProvider::new(base_url, "test-key", "test-model");

        let err = provider
            .complete("system", "say hello")
            .expect_err("null content should be reported, not panic");
        assert!(matches!(err, ProviderError::UnexpectedResponse(_)));
    }

    #[test]
    fn connection_errors_include_the_real_cause_not_just_the_generic_wrapper() {
        // Bind a listener to get a genuinely free local port, then drop
        // it immediately, so nothing is actually listening there
        // anymore. Connecting to a free port on localhost reliably
        // fails with "connection refused", without depending on a real
        // server being reachable (or unreachable) in whatever
        // environment runs this test.
        //
        // Regression test for a real bug a maintainer hit while
        // debugging a local Ollama connection: `reqwest::Error`'s own
        // `Display` text only says "error sending request for url
        // (...)", with no hint of *why*. The real reason lives in its
        // `source()` chain instead.
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let addr = listener
            .local_addr()
            .expect("mock server should have an address");
        drop(listener);

        let provider =
            OpenAiCompatibleProvider::new(format!("http://{addr}"), "test-key", "test-model");
        let err = provider
            .complete("system", "say hello")
            .expect_err("nothing is listening on this port, so this must fail");
        match err {
            ProviderError::Request(message) => {
                assert!(
                    message.to_lowercase().contains("refused"),
                    "expected the real connection failure reason (\"connection refused\"), \
                     not just the generic wrapper text; got: {message}"
                );
            }
            other => panic!("expected ProviderError::Request, got {other:?}"),
        }
    }

    #[test]
    fn oversized_response_is_rejected() {
        let huge_body = "x".repeat(MAX_RESPONSE_BYTES + 1);
        let base_url = mock_server_once("HTTP/1.1 200 OK", huge_body);
        let provider = OpenAiCompatibleProvider::new(base_url, "test-key", "test-model");

        let err = provider
            .complete("system", "say hello")
            .expect_err("an oversized response should be rejected");
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

        let reply = provider
            .complete("system", "say hello")
            .expect("should succeed");
        assert_eq!(reply, "hello from claude");
    }

    #[test]
    fn anthropic_joins_every_text_block_not_only_the_first() {
        let base_url = mock_server_once(
            "HTTP/1.1 200 OK",
            r#"{"content":[{"type":"text","text":"first block"},{"type":"text","text":"second block"}]}"#
                .to_string(),
        );
        let provider =
            AnthropicProvider::with_base_url(base_url, "test-key", "claude-3-5-haiku-20241022");

        let reply = provider
            .complete("system", "say hello")
            .expect("should succeed");
        assert_eq!(reply, "first block\nsecond block");
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

        let err = provider
            .complete("system", "say hello")
            .expect_err("should fail");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "slow down");
            }
            other => panic!("expected ProviderError::Api, got {other:?}"),
        }
    }

    #[test]
    fn join_url_avoids_a_double_slash() {
        assert_eq!(
            join_url("https://example.com/v1/", "chat/completions"),
            "https://example.com/v1/chat/completions"
        );
        assert_eq!(
            join_url("https://example.com/v1", "/chat/completions"),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn truncate_for_display_leaves_short_text_unchanged() {
        assert_eq!(truncate_for_display("short"), "short");
    }

    #[test]
    fn truncate_for_display_shortens_long_text() {
        let long = "a".repeat(MAX_QUOTED_BODY_CHARS + 50);
        let truncated = truncate_for_display(&long);
        assert!(truncated.len() < long.len());
        assert!(truncated.ends_with("(truncated)"));
    }
}
