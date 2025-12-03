//! Types for AI Core provider configuration

use serde::{Deserialize, Serialize};

/// Specifies which vendor API type to use for an AI Core deployment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiCoreApiType {
    /// Anthropic Claude API (Bedrock-style invoke/converse endpoints)
    #[default]
    Anthropic,
    /// OpenAI Chat Completions API
    OpenAI,
    /// Google Vertex AI / Gemini API
    Vertex,
}

impl std::fmt::Display for AiCoreApiType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiCoreApiType::Anthropic => write!(f, "anthropic"),
            AiCoreApiType::OpenAI => write!(f, "openai"),
            AiCoreApiType::Vertex => write!(f, "vertex"),
        }
    }
}

impl std::str::FromStr for AiCoreApiType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "anthropic" => Ok(AiCoreApiType::Anthropic),
            "openai" => Ok(AiCoreApiType::OpenAI),
            "vertex" => Ok(AiCoreApiType::Vertex),
            _ => Err(anyhow::anyhow!(
                "Unknown AI Core API type: '{}'. Expected one of: anthropic, openai, vertex",
                s
            )),
        }
    }
}
