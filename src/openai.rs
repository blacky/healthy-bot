use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Sanitize a display name for OpenAI's chat `name` field.
///
/// OpenAI requires `name` to match `^[a-zA-Z0-9_-]+$` (max 64 chars). Discord
/// usernames routinely contain spaces, dots, and Unicode, which causes the API
/// to reject the whole request with a 400. Disallowed characters are replaced
/// with `_`; if nothing valid remains, we fall back to `"user"`.
pub fn sanitize_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();

    if sanitized.is_empty() {
        "user".to_string()
    } else {
        sanitized
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub name: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatMessageContent,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessageContent {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ImageRequest {
    pub prompt: String,
    pub user: String,
    pub size: String,
}

#[derive(Debug, Deserialize)]
pub struct ImageResponse {
    pub data: Vec<ImageUrl>,
}

#[derive(Debug, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug)]
pub struct OpenAIClient {
    pub client: Client,
    pub api_key: String,
}

impl OpenAIClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                // Falling back to Client::default() would silently drop the
                // timeout, risking a hung request stalling the bot. A build
                // failure here means TLS init is broken, so fail loudly instead.
                .expect("Failed to build OpenAI HTTP client"),
            api_key,
        }
    }

    pub async fn create_chat(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatResponse, crate::Error> {
        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&ChatRequest {
                model: model.to_string(),
                messages,
            })
            .send()
            .await?;

        parse_response(response).await
    }

    pub async fn create_image(
        &self,
        prompt: &str,
        user: &str,
    ) -> Result<ImageResponse, crate::Error> {
        let response = self
            .client
            .post("https://api.openai.com/v1/images/generations")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&ImageRequest {
                prompt: prompt.to_string(),
                user: user.to_string(),
                size: "1024x1024".to_string(),
            })
            .send()
            .await?;

        parse_response(response).await
    }
}

/// Turn a raw response into the expected payload, surfacing API errors clearly.
///
/// On a non-2xx status, OpenAI returns an error-shaped JSON body that would
/// otherwise fail to deserialize into the success type and produce a misleading
/// serde error. We check the status first and include the real status + body.
async fn parse_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, crate::Error> {
    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        return Err(format!("OpenAI API returned {}: {}", status, body.trim()).into());
    }

    serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse OpenAI response ({}): {}", e, body.trim()).into())
}
