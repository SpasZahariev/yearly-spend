use std::path::PathBuf;

pub const GEMINI_API_BASE: &str = "https://geminiapi.googleapis.com/v1beta";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Local,
    Gemini,
}

impl LlmProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: PathBuf,
    pub llm_provider: LlmProvider,
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    pub gemini_base_url: String,
    pub fx_base_url: String,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();
        let get = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());

        let provider = get("LLM_PROVIDER")
            .as_deref()
            .and_then(LlmProvider::parse)
            .unwrap_or(LlmProvider::Local);

        Ok(Self {
            db_path: get("DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data/spend.duckdb")),
            llm_provider: provider,
            llm_base_url: get("LLM_BASE_URL")
                .unwrap_or_else(|| "http://localhost:11434/v1".to_string()),
            llm_api_key: get("LLM_API_KEY").unwrap_or_else(|| "sk-local-token".to_string()),
            llm_model: get("LLM_MODEL").unwrap_or_default(),
            gemini_api_key: get("GEMINI_API_KEY"),
            gemini_model: get("GEMINI_MODEL").unwrap_or_else(|| "gemini-3.5-flash".to_string()),
            gemini_base_url: get("GEMINI_BASE_URL")
                .map(|v| v.trim_end_matches('/').to_string())
                .unwrap_or_else(|| GEMINI_API_BASE.to_string()),
            fx_base_url: get("FX_BASE_URL")
                .unwrap_or_else(|| "https://api.frankfurter.dev".to_string()),
        })
    }

    pub fn llm_model_or_err(&self) -> anyhow::Result<&str> {
        anyhow::ensure!(!self.llm_model.is_empty(), "LLM_MODEL is not set");
        Ok(&self.llm_model)
    }
}

/// The effective LLM model id for the configured provider.
pub fn effective_model(cfg: &Config) -> String {
    match cfg.llm_provider {
        LlmProvider::Local => cfg.llm_model.clone(),
        LlmProvider::Gemini => {
            if cfg.gemini_model.is_empty() {
                cfg.llm_model.clone()
            } else {
                cfg.gemini_model.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parsing() {
        assert_eq!(LlmProvider::parse("local"), Some(LlmProvider::Local));
        assert_eq!(LlmProvider::parse("Gemini"), Some(LlmProvider::Gemini));
        assert_eq!(LlmProvider::parse("nope"), None);
    }

    fn test_config(provider: LlmProvider, llm_model: &str, gemini_model: &str) -> Config {
        Config {
            db_path: PathBuf::from("data/spend.duckdb"),
            llm_provider: provider,
            llm_base_url: "http://localhost:11434/v1".into(),
            llm_api_key: "sk-local-token".into(),
            llm_model: llm_model.into(),
            gemini_api_key: None,
            gemini_model: gemini_model.into(),
            gemini_base_url: GEMINI_API_BASE.into(),
            fx_base_url: "https://api.frankfurter.dev".into(),
        }
    }

    #[test]
    fn effective_model_uses_local_model_for_local_provider() {
        let cfg = test_config(LlmProvider::Local, "local-model", "gemini-3.5-flash");
        assert_eq!(effective_model(&cfg), "local-model");
    }

    #[test]
    fn effective_model_prefers_gemini_model_when_set() {
        let cfg = test_config(LlmProvider::Gemini, "local-model", "gemini-3.5-flash");
        assert_eq!(effective_model(&cfg), "gemini-3.5-flash");
    }

    #[test]
    fn effective_model_falls_back_to_local_when_gemini_empty() {
        let cfg = test_config(LlmProvider::Gemini, "local-model", "");
        assert_eq!(effective_model(&cfg), "local-model");
    }
}
