//! Retrieval hybride (Intelligence §6) : SQL structuré + sémantique (vecteurs),
//! fusion & ranking, budget de tokens strict, assemblage SOURCÉ.
//! Règle d'or : un bon retrieval bat un gros modèle.

use crate::db::Db;
use crate::error::Result;
use crate::llm::{blob_to_vec, cosine, LlmClient};
use crate::security::provenance;
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct Retrieved {
    pub item_id: String,
    pub source: String,
    pub source_ref: String,
    pub title: String,
    pub path: Option<String>,
    pub snippet: String,
    pub score: f32,
}

const CONTEXT_CHAR_BUDGET: usize = 9000; // budget strict (dépend du palier modèle)
const TOP_N: usize = 8;

fn keywords(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Recherche hybride sur la mémoire sémantique + structurée.
pub async fn search(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
) -> Result<Vec<Retrieved>> {
    let kws = keywords(query);
    let mut scores: HashMap<String, Retrieved> = HashMap::new();
    let now = crate::db::now();

    // — Voie structurée (SQL) : mots-clés sur titre/corps/chemin —
    if !kws.is_empty() {
        let like_clauses: Vec<String> = (0..kws.len())
            .map(|i| {
                format!(
                    "(lower(title) LIKE '%'||?{0}||'%' OR lower(body) LIKE '%'||?{0}||'%' OR lower(path) LIKE '%'||?{0}||'%')",
                    i + 1
                )
            })
            .collect();
        let sql = format!(
            "SELECT id, source, source_ref, title, path, substr(COALESCE(body, ''), 1, 400), mtime,
                    ({}) AS hits
             FROM items WHERE status='active' AND ({})
             ORDER BY hits DESC LIMIT 40",
            like_clauses
                .iter()
                .map(|c| format!("CASE WHEN {c} THEN 1 ELSE 0 END"))
                .collect::<Vec<_>>()
                .join(" + "),
            like_clauses.join(" OR ")
        );
        db.with(|c| {
            let mut stmt = c.prepare(&sql)?;
            let params_vec: Vec<&dyn rusqlite::ToSql> =
                kws.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
            let rows = stmt.query_map(params_vec.as_slice(), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, source, source_ref, title, path, snippet, mtime, hits) = row?;
                let recency = recency_boost(now, mtime);
                let score = 0.12 * hits as f32 + 0.2 * recency;
                scores.insert(
                    id.clone(),
                    Retrieved {
                        item_id: id,
                        source,
                        source_ref,
                        title: title.unwrap_or_default(),
                        path,
                        snippet,
                        score,
                    },
                );
            }
            Ok(())
        })?;
    }

    // — Voie sémantique (vecteurs) — dégradation gracieuse si embeddings indisponibles.
    if let Ok(qvecs) = llm.embed(&[query.to_string()]).await {
        if let Some(qvec) = qvecs.first() {
            let rows: Vec<(String, String, Vec<u8>)> = db.with(|c| {
                let mut stmt = c.prepare(
                    "SELECT e.item_id, e.text, e.vector FROM embeddings e
                     JOIN items i ON i.id = e.item_id
                     WHERE e.vector IS NOT NULL AND i.status = 'active'",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                let mut out = vec![];
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })?;

            // Meilleur chunk par item.
            let mut best: HashMap<String, (f32, String)> = HashMap::new();
            for (item_id, text, blob) in rows {
                let sim = cosine(qvec, &blob_to_vec(&blob));
                let entry = best.entry(item_id).or_insert((f32::MIN, String::new()));
                if sim > entry.0 {
                    *entry = (sim, text);
                }
            }
            if !best.is_empty() {
                type ItemMeta = (
                    String,
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                    Option<i64>,
                );
                let metas: Vec<ItemMeta> =
                    db.with(|c| {
                        let mut stmt = c.prepare(
                            "SELECT id, source, source_ref, title, path, mtime FROM items WHERE id = ?1",
                        )?;
                        let mut out = vec![];
                        for id in best.keys() {
                            if let Ok(m) = stmt.query_row(params![id], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, Option<String>>(3)?,
                                    r.get::<_, Option<String>>(4)?,
                                    r.get::<_, Option<i64>>(5)?,
                                ))
                            }) {
                                out.push(m);
                            }
                        }
                        Ok(out)
                    })?;
                for (id, source, source_ref, title, path, mtime) in metas {
                    let (sim, text) = best.get(&id).cloned().unwrap_or((0.0, String::new()));
                    if sim < 0.35 {
                        continue; // bruit
                    }
                    let add = 0.65 * sim + 0.2 * recency_boost(now, mtime);
                    scores
                        .entry(id.clone())
                        .and_modify(|r| {
                            r.score += add;
                            if !text.is_empty() {
                                r.snippet = text.chars().take(600).collect();
                            }
                        })
                        .or_insert(Retrieved {
                            item_id: id,
                            source,
                            source_ref,
                            title: title.unwrap_or_default(),
                            path,
                            snippet: text.chars().take(600).collect(),
                            score: add,
                        });
                }
            }
        }
    }

    let mut out: Vec<Retrieved> = scores.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit.max(1));
    Ok(out)
}

fn recency_boost(now: i64, mtime: Option<i64>) -> f32 {
    match mtime {
        Some(t) => {
            let days = ((now - t).max(0) as f32) / 86_400.0;
            (1.0 / (1.0 + days / 30.0)).min(1.0)
        }
        None => 0.2,
    }
}

pub struct ContextBundle {
    /// (index de citation, fragment enveloppé « donnée non fiable »)
    pub fragments: Vec<(usize, String)>,
    pub sources: Vec<Retrieved>,
    /// Concaténation brute du contenu non fiable (pour l'analyse de dérivation).
    pub untrusted_text: String,
}

/// Assemblage borné + sourcé : chaque fragment garde son source_ref pour la citation.
pub async fn assemble(db: &Db, llm: &Arc<dyn LlmClient>, query: &str) -> Result<ContextBundle> {
    let results = search(db, llm, query, TOP_N).await?;
    let mut fragments = vec![];
    let mut sources = vec![];
    let mut untrusted = String::new();
    let mut budget = CONTEXT_CHAR_BUDGET;
    for (i, r) in results.into_iter().enumerate() {
        let text = format!(
            "[source:{}] {} — {}\n{}",
            i + 1,
            r.title,
            r.source_ref,
            r.snippet
        );
        if text.len() > budget {
            break;
        }
        budget -= text.len();
        untrusted.push_str(&r.snippet);
        untrusted.push('\n');
        fragments.push((i + 1, provenance::wrap_untrusted(&r.source_ref, &text)));
        sources.push(r);
    }
    Ok(ContextBundle {
        fragments,
        sources,
        untrusted_text: untrusted,
    })
}
