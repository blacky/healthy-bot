use serde::{Serialize, Deserialize};
use reqwest::Client;

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
                .unwrap_or_default(),
            api_key,
        }
    }

    pub async fn create_chat(&self, model: &str, messages: Vec<ChatMessage>) -> reqwest::Result<ChatResponse> {
        self.client.post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&ChatRequest {
                model: model.to_string(),
                messages,
            })
            .send()
            .await?
            .json()
            .await
    }

    pub async fn create_image(&self, prompt: &str, user: &str) -> reqwest::Result<ImageResponse> {
        self.client.post("https://api.openai.com/v1/images/generations")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&ImageRequest {
                prompt: prompt.to_string(),
                user: user.to_string(),
                size: "1024x1024".to_string(),
            })
            .send()
            .await?
            .json()
            .await
    }
}
