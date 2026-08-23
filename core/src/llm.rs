use serde::{Deserialize, Serialize};

use crate::config::{Config, LlmProvider};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// OpenAI-compatible chat client routing to the configured provider. The local
/// llama.cpp server is the default; Gemini activates only when
/// `LLM_PROVIDER=gemini` with an API key present.
pub struct Llm {
    http: reqwest::Client,
    provider: LlmProvider,
    base_url: String,
    api_key: String,
    model: String,
    gemini_base_url: String,
}

#[derive(Serialize)]
struct LocalRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    temperature: f32,
    /// Qwen3 thinking mode emits reasoning tokens; the spec requires it off.
    #[serde(rename = "chat_template_kwargs")]
    chat_template_kwargs: ChatTemplate,
}

#[derive(Serialize)]
struct ChatTemplate {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    generation_config: GeminiGenerationConfig,
}

#[derive(Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiGenerationConfig {
    temperature: f32,
}

#[derive(Deserialize)]
struct LocalResponse {
    choices: Vec<LocalChoice>,
}

#[derive(Deserialize)]
struct LocalChoice {
    message: LocalMessage,
}

#[derive(Deserialize)]
struct LocalMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiOutContent,
}

#[derive(Deserialize)]
struct GeminiOutContent {
    parts: Vec<GeminiOutPart>,
}

#[derive(Deserialize)]
struct GeminiOutPart {
    text: String,
}

impl Llm {
    pub fn new(cfg: &Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            provider: cfg.llm_provider,
            base_url: cfg.llm_base_url.trim_end_matches('/').to_string(),
            api_key: cfg.llm_api_key.clone(),
            model: crate::config::effective_model(cfg),
            gemini_base_url: cfg.gemini_base_url.trim_end_matches('/').to_string(),
        }
    }

    pub fn provider(&self) -> LlmProvider {
        self.provider
    }

    fn require_model(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.model.is_empty(), "LLM model is not configured");
        Ok(())
    }

    /// One chat completion against the configured provider.
    pub async fn complete(&self, messages: &[Message]) -> anyhow::Result<String> {
        match self.provider {
            LlmProvider::Local => self.complete_local(messages).await,
            LlmProvider::Gemini => self.complete_gemini(messages).await,
        }
    }

    async fn complete_local(&self, messages: &[Message]) -> anyhow::Result<String> {
        self.require_model()?;
        let body = LocalRequest {
            model: &self.model,
            messages,
            temperature: 0.2,
            chat_template_kwargs: ChatTemplate {
                enable_thinking: false,
            },
        };
        let response = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let parsed: LocalResponse = response.json().await?;
        anyhow::ensure!(!parsed.choices.is_empty(), "no choices in LLM response");
        let content = parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow::anyhow!("empty LLM completion"))?;
        Ok(content)
    }

    async fn complete_gemini(&self, messages: &[Message]) -> anyhow::Result<String> {
        self.require_model()?;
        anyhow::ensure!(
            !self.api_key.is_empty(),
            "GEMINI_API_KEY is not set for provider 'gemini'"
        );
        let mut system = None;
        let mut contents = Vec::new();
        for m in messages {
            match m.role.as_str() {
                "system" => {
                    system = Some(GeminiContent {
                        role: "user".into(),
                        parts: vec![GeminiPart {
                            text: m.content.clone(),
                        }],
                    })
                }
                "assistant" => contents.push(GeminiContent {
                    role: "model".into(),
                    parts: vec![GeminiPart {
                        text: m.content.clone(),
                    }],
                }),
                _ => contents.push(GeminiContent {
                    role: "user".into(),
                    parts: vec![GeminiPart {
                        text: m.content.clone(),
                    }],
                }),
            }
        }
        let body = GeminiRequest {
            contents,
            system_instruction: system,
            generation_config: GeminiGenerationConfig { temperature: 0.2 },
        };
        let response = self
            .http
            .post(format!(
                "{}/models/{}:generateContent",
                self.gemini_base_url, self.model
            ))
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let parsed: GeminiResponse = response.json().await?;
        anyhow::ensure!(
            !parsed.candidates.is_empty(),
            "no candidates in Gemini response"
        );
        let text: String = parsed
            .candidates
            .first()
            .map(|c| {
                c.content
                    .parts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<String>()
            })
            .unwrap_or_default();
        anyhow::ensure!(!text.is_empty(), "empty Gemini response");
        Ok(text)
    }
}
