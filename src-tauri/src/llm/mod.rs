//! La couche d'intelligence (deep-dive Intelligence §8) : tout passe par `LlmClient`.
//! Le modèle est un composant remplaçable ; l'intelligence de Syn est le système.

pub mod ollama;
pub mod profiles;

use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // system | user | assistant | tool
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }
    pub fn tool(name: &str, content: impl Into<String>) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: Some(name.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

/// Contrat d'outil (doc maître §10) — exposé au routeur.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip)]
    pub side_effect: SideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    Read,
    WriteLocal,
    WriteExternal,
}

#[derive(Debug, Clone, Default)]
pub struct GenParams {
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmStatus {
    pub available: bool,
    pub runtime: String,
    pub chat_model_ready: bool,
    pub embed_model_ready: bool,
    pub installed_models: Vec<String>,
    pub detail: Option<String>,
}

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        params: GenParams,
    ) -> Result<LlmResponse>;

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    async fn status(&self) -> LlmStatus;

    /// Téléchargement d'un modèle (onboarding étape « mise en route ») — reprenable.
    async fn pull(
        &self,
        model: &str,
        progress: tokio::sync::mpsc::Sender<(f32, String)>,
    ) -> Result<()>;
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

pub fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
