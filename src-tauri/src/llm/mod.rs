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


/// Récupère un appel d'outil que le modèle a écrit en TEXTE au lieu de le
/// livrer dans le champ structuré.
///
/// Llama le fait régulièrement, et parfois avec un JSON abîmé
/// (`"parameters{"to":…` — les deux-points manquent). Sans ce rattrapage,
/// l'utilisateur voyait le JSON brut s'afficher dans la conversation à la place
/// de l'action demandée. On ne cherche donc pas à valider du JSON parfait : on
/// extrait le nom de l'outil et le premier objet équilibré qui suit
/// `parameters` ou `arguments`.
pub fn tool_call_from_text(content: &str) -> Option<ToolCall> {
    let body = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if !body.starts_with('{') {
        return None;
    }

    // Voie normale : le JSON est valide.
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let function = if value["function"].is_object() {
            value["function"].clone()
        } else {
            value.clone()
        };
        if let Some(name) = function["name"].as_str() {
            let arguments = ["arguments", "parameters"]
                .iter()
                .map(|key| function[*key].clone())
                .find(|value| value.is_object())
                .unwrap_or_else(|| serde_json::json!({}));
            return Some(ToolCall {
                name: name.to_string(),
                arguments,
            });
        }
    }

    // Voie de secours : JSON abîmé. On lit les deux seules informations utiles.
    let name = quoted_value_after(body, "\"name\"")?;
    let arguments = ["parameters", "arguments"]
        .iter()
        .filter_map(|key| balanced_object_after(body, key))
        .find_map(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    Some(ToolCall { name, arguments })
}

/// Valeur entre guillemets qui suit une clé, sans exiger la ponctuation JSON.
fn quoted_value_after(text: &str, key: &str) -> Option<String> {
    let after = &text[text.find(key)? + key.len()..];
    let start = after.find('"')?;
    let rest = &after[start + 1..];
    let end = rest.find('"')?;
    let value = &rest[..end];
    (!value.is_empty()).then(|| value.to_string())
}

/// Premier objet `{…}` équilibré qui suit une clé, guillemets pris en compte.
fn balanced_object_after(text: &str, key: &str) -> Option<String> {
    let from = text.find(key)? + key.len();
    let start = from + text[from..].find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in text[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + offset + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Le contenu ressemble-t-il à une sortie structurée plutôt qu'à une phrase ?
/// Sert à ne jamais diffuser de JSON dans la conversation.
pub fn looks_structured(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with("```")
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

    /// Génère en diffusant les fragments au fil de l'eau dans `sink`.
    ///
    /// Le résultat final est identique à `generate` : seul le RESSENTI change,
    /// et c'est justement ce que l'utilisateur mesure. Implémentation par
    /// défaut : pas de diffusion, un seul bloc à la fin — un runtime qui ne
    /// sait pas diffuser reste utilisable.
    async fn generate_streaming(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        params: GenParams,
        _sink: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<LlmResponse> {
        self.generate(system, messages, tools, params).await
    }

    /// Charge les modèles en mémoire sans rien produire. Appelé au démarrage :
    /// le coût de chargement (plusieurs secondes pour un modèle de 5 Go) est
    /// alors payé pendant que l'utilisateur découvre l'interface, et non au
    /// milieu de sa première question.
    ///
    /// Implémentation par défaut : sans objet pour les runtimes sans cache.
    async fn warm_up(&self) {}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Cas réel du 17/08/2026 : Syn affichait ce JSON dans la conversation au
    /// lieu d'envoyer le mail. Noter le `"parameters{` — les deux-points
    /// manquent, le JSON est invalide.
    #[test]
    fn un_appel_doutil_ecrit_en_texte_est_recupere_meme_abime() {
        let abime = r#"{"type":"function","name":"mail.send","parameters{"to":"paul@example.com","subject":"Bonjour","body":"Ceci est un test de mail."}}}"#;
        let call = tool_call_from_text(abime).expect("appel non récupéré");
        assert_eq!(call.name, "mail.send");
        assert_eq!(call.arguments["to"], "paul@example.com");
        assert_eq!(call.arguments["subject"], "Bonjour");
    }

    #[test]
    fn les_formes_valides_habituelles_passent_aussi() {
        for texte in [
            r#"{"name":"files.search","arguments":{"query":"bail"}}"#,
            r#"{"function":{"name":"files.search","arguments":{"query":"bail"}}}"#,
            "```json\n{\"type\":\"function\",\"name\":\"files.search\",\"parameters\":{\"query\":\"bail\"}}\n```",
        ] {
            let call = tool_call_from_text(texte).expect(texte);
            assert_eq!(call.name, "files.search");
            assert_eq!(call.arguments["query"], "bail");
        }
    }

    #[test]
    fn une_vraie_phrase_nest_jamais_prise_pour_un_appel_doutil() {
        assert!(tool_call_from_text("J'ai trouvé deux documents.").is_none());
        assert!(!looks_structured("J'ai trouvé deux documents."));
        assert!(looks_structured("{\"name\":"));
        assert!(looks_structured("```json"));
    }
}
