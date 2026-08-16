//! Runtime de dev : Ollama en HTTP loopback (décision actée, Intelligence §3).
//! En prod : moteur embarqué derrière la même interface (🔎 candle / mistral.rs /
//! llama.cpp — à figer). Le reste de Syn ignore quelle implémentation est active.

use super::*;
use crate::error::AppError;
use crate::security::egress::EgressGuard;
use futures_util::StreamExt;
use serde_json::json;
use std::sync::Arc;

pub struct OllamaClient {
    base: String,
    chat_model: String,
    embed_model: String,
    http: reqwest::Client,
    egress: Arc<EgressGuard>,
}

impl OllamaClient {
    pub fn new(base: &str, chat_model: &str, embed_model: &str, egress: Arc<EgressGuard>) -> Self {
        OllamaClient {
            base: base.trim_end_matches('/').to_string(),
            chat_model: chat_model.to_string(),
            embed_model: embed_model.to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                // Ne jamais laisser un serveur loopback autorisé rediriger
                // silencieusement la requête vers un hôte externe.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client http"),
            egress,
        }
    }

    fn url(&self, path: &str) -> crate::error::Result<String> {
        let url = format!("{}{}", self.base, path);
        self.egress.check(&url)?;
        Ok(url)
    }
}

#[async_trait::async_trait]
impl LlmClient for OllamaClient {
    async fn generate(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        params: GenParams,
    ) -> Result<LlmResponse> {
        let mut msgs = vec![json!({"role": "system", "content": system})];
        for m in messages {
            let mut o = json!({"role": m.role, "content": m.content});
            if m.role == "tool" {
                if let Some(name) = &m.tool_name {
                    o["tool_name"] = json!(name);
                }
            }
            if let Some(calls) = &m.tool_calls {
                o["tool_calls"] = json!(calls
                    .iter()
                    .map(|c| json!({"function": {"name": c.name, "arguments": c.arguments}}))
                    .collect::<Vec<_>>());
            }
            msgs.push(o);
        }
        let mut body = json!({
            "model": self.chat_model,
            "messages": msgs,
            "stream": false,
            "options": { "temperature": params.temperature }
        });
        if let Some(mt) = params.max_tokens {
            body["options"]["num_predict"] = json!(mt);
        }
        if params.json {
            body["format"] = json!("json");
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema
                    }
                }))
                .collect::<Vec<_>>());
        }

        let url = self.url("/api/chat")?;
        let resp = self.http.post(&url).json(&body).send().await.map_err(|e| {
            AppError::Llm(format!(
                "Le moteur local est indisponible ({e}). Les réponses générées sont momentanément désactivées."
            ))
        })?;
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Llm(format!("réponse du runtime {code} : {text}")));
        }
        let v: serde_json::Value = resp.json().await?;
        let message = &v["message"];
        let mut tool_calls = vec![];
        if let Some(calls) = message["tool_calls"].as_array() {
            for c in calls {
                let f = &c["function"];
                if let Some(name) = f["name"].as_str() {
                    // Les arguments arrivent en objet ou en chaîne JSON selon le modèle.
                    let args = if f["arguments"].is_string() {
                        serde_json::from_str(f["arguments"].as_str().unwrap()).unwrap_or(json!({}))
                    } else {
                        f["arguments"].clone()
                    };
                    tool_calls.push(ToolCall {
                        name: name.to_string(),
                        arguments: args,
                    });
                }
            }
        }
        Ok(LlmResponse {
            content: message["content"].as_str().unwrap_or("").to_string(),
            tool_calls,
        })
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = self.url("/api/embed")?;
        let resp = self
            .http
            .post(&url)
            .json(&json!({"model": self.embed_model, "input": texts}))
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("embeddings indisponibles : {e}")))?;
        if !resp.status().is_success() {
            let t = resp.text().await.unwrap_or_default();
            return Err(AppError::Llm(format!("embeddings : {t}")));
        }
        let v: serde_json::Value = resp.json().await?;
        let arr = v["embeddings"]
            .as_array()
            .ok_or_else(|| AppError::Llm("réponse d'embedding invalide".into()))?;
        Ok(arr
            .iter()
            .map(|e| {
                e.as_array()
                    .map(|xs| {
                        xs.iter()
                            .filter_map(|x| x.as_f64())
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect())
    }

    async fn status(&self) -> LlmStatus {
        let url = match self.url("/api/tags") {
            Ok(u) => u,
            Err(e) => {
                return LlmStatus {
                    available: false,
                    runtime: "ollama".into(),
                    chat_model_ready: false,
                    embed_model_ready: false,
                    installed_models: vec![],
                    detail: Some(e.to_string()),
                }
            }
        };
        match self
            .http
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => {
                let v: serde_json::Value = resp.json().await.unwrap_or(json!({}));
                let models: Vec<String> = v["models"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m["name"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let has = |name: &str| {
                    let short = name.split(':').next().unwrap_or(name);
                    models
                        .iter()
                        .any(|m| m == name || m.split(':').next() == Some(short))
                };
                LlmStatus {
                    available: true,
                    runtime: "ollama".into(),
                    chat_model_ready: has(&self.chat_model),
                    embed_model_ready: has(&self.embed_model),
                    installed_models: models,
                    detail: None,
                }
            }
            Err(e) => LlmStatus {
                available: false,
                runtime: "ollama".into(),
                chat_model_ready: false,
                embed_model_ready: false,
                installed_models: vec![],
                detail: Some(format!("Ollama injoignable : {e}")),
            },
        }
    }

    async fn pull(
        &self,
        model: &str,
        progress: tokio::sync::mpsc::Sender<(f32, String)>,
    ) -> Result<()> {
        let url = self.url("/api/pull")?;
        let resp = self
            .http
            .post(&url)
            .json(&json!({"model": model, "stream": true}))
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("téléchargement impossible : {e}")))?;
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| AppError::Llm(format!("téléchargement interrompu : {e}")))?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) {
                    let status = v["status"].as_str().unwrap_or("").to_string();
                    let pct = match (v["completed"].as_f64(), v["total"].as_f64()) {
                        (Some(c), Some(t)) if t > 0.0 => (c / t * 100.0) as f32,
                        _ => -1.0,
                    };
                    if let Some(err) = v["error"].as_str() {
                        return Err(AppError::Llm(err.to_string()));
                    }
                    let _ = progress.send((pct, status)).await;
                }
            }
        }
        Ok(())
    }
}
