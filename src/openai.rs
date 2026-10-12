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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatMessageRequestContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl std::fmt::Display for ChatMessageRequestContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(s) => f.write_str(s),
            Self::Parts(parts) => {
                let mut first = true;
                for part in parts {
                    if let ContentPart::Text { text } = part {
                        if !first {
                            f.write_str(" ")?;
                        }
                        f.write_str(text)?;
                        first = false;
                    }
                }
                if first {
                    f.write_str("[Image/Media]")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlTarget },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlTarget {
    pub url: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub name: Option<String>,
    pub content: ChatMessageRequestContent,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChatChoice {
    pub message: ChatMessageContent,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChatMessageContent {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub refusal: Option<String>,
}

impl ChatMessageContent {
    /// Returns the text content if present and non-empty, otherwise `""`.
    pub fn content_text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
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

#[derive(Debug, Clone)]
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
        self.create_chat_with_max_tokens(model, messages, None)
            .await
    }

    pub async fn create_chat_with_max_tokens(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        max_tokens: Option<u32>,
    ) -> Result<ChatResponse, crate::Error> {
        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&ChatRequest {
                model: model.to_string(),
                messages,
                max_completion_tokens: max_tokens,
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

/// Build chat message request content with vision support, downloading attachments
/// and base64-encoding images when vision is enabled.
pub async fn build_message_content(
    client: &reqwest::Client,
    text: &str,
    attachments: &[poise::serenity_prelude::Attachment],
    supports_vision: bool,
) -> ChatMessageRequestContent {
    if !supports_vision {
        return ChatMessageRequestContent::Text(text.to_string());
    }

    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(ContentPart::Text {
            text: text.to_string(),
        });
    }

    let mut image_count = 0;

    // 1. Process attachments
    for attachment in attachments {
        if image_count >= 2 {
            break;
        }

        let is_image = attachment
            .content_type
            .as_deref()
            .map(|ct| ct.starts_with("image/"))
            .unwrap_or_else(|| {
                let name = attachment.filename.to_lowercase();
                name.ends_with(".png")
                    || name.ends_with(".jpg")
                    || name.ends_with(".jpeg")
                    || name.ends_with(".webp")
                    || name.ends_with(".gif")
            });

        if is_image {
            match attachment.download().await {
                Ok(bytes) => {
                    use base64::{engine::general_purpose, Engine as _};
                    let base64_data = general_purpose::STANDARD.encode(&bytes);
                    let mime_type = attachment.content_type.as_deref().unwrap_or("image/jpeg");
                    let data_url = format!("data:{};base64,{}", mime_type, base64_data);
                    parts.push(ContentPart::ImageUrl {
                        image_url: ImageUrlTarget {
                            url: data_url,
                            detail: Some("low".to_string()),
                        },
                    });
                    image_count += 1;
                }
                Err(e) => {
                    log::error!(
                        "Failed to download attachment {}: {:?}",
                        attachment.filename,
                        e
                    );
                }
            }
        }
    }

    // 2. Process image URLs from text
    if image_count < 2 {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let regex = RE.get_or_init(|| {
            regex::Regex::new(r"(?i)https?://\S+?\.(?:png|jpg|jpeg|webp|gif)(?:\?\S*)?").unwrap()
        });

        for mat in regex.find_iter(text) {
            if image_count >= 2 {
                break;
            }

            let url = mat.as_str();
            match client.get(url).send().await {
                Ok(resp) => {
                    let content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|val| val.to_str().ok())
                        .map(|s| s.to_string());

                    let is_image = content_type
                        .as_deref()
                        .map(|ct| ct.starts_with("image/"))
                        .unwrap_or(false);

                    if is_image {
                        match resp.bytes().await {
                            Ok(bytes) => {
                                use base64::{engine::general_purpose, Engine as _};
                                let base64_data = general_purpose::STANDARD.encode(&bytes);
                                let mime_type =
                                    content_type.unwrap_or_else(|| "image/jpeg".to_string());
                                let data_url = format!("data:{};base64,{}", mime_type, base64_data);
                                parts.push(ContentPart::ImageUrl {
                                    image_url: ImageUrlTarget {
                                        url: data_url,
                                        detail: Some("low".to_string()),
                                    },
                                });
                                image_count += 1;
                            }
                            Err(e) => {
                                log::error!("Failed to read bytes from image URL {}: {:?}", url, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to fetch image URL {}: {:?}", url, e);
                }
            }
        }
    }

    if parts.is_empty() {
        ChatMessageRequestContent::Text(text.to_string())
    } else if parts.len() == 1 {
        match parts.remove(0) {
            ContentPart::Text { text } => ChatMessageRequestContent::Text(text),
            other => ChatMessageRequestContent::Parts(vec![other]),
        }
    } else {
        ChatMessageRequestContent::Parts(parts)
    }
}
