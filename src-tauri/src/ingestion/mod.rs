//! Pipeline d'ingestion commun (Files §4) : normaliser → chunker → embedder →
//! écrire items/embeddings → extraire les entités. Le contenu ingéré est une
//! DONNÉE non fiable (Sécurité §2) — jamais une instruction.

pub mod extract;

use crate::bus::{Bus, BusEvent};
use crate::db::Db;
use crate::error::Result;
use crate::llm::{vec_to_blob, LlmClient};
use crate::memory::{self, Item};
use std::sync::Arc;

/// Découpe en fragments cohérents (~1200 caractères, chevauchement léger).
pub fn chunk(text: &str) -> Vec<String> {
    const TARGET: usize = 1200;
    const MAX_CHUNKS: usize = 64;
    let mut chunks = vec![];
    let mut current = String::new();
    for para in text.split("\n\n") {
        if current.len() + para.len() > TARGET && !current.is_empty() {
            chunks.push(current.trim().to_string());
            current.clear();
            if chunks.len() >= MAX_CHUNKS {
                break;
            }
        }
        if para.len() > TARGET * 2 {
            // Paragraphe géant : découpe dure par phrases approximatives.
            let mut buf = String::new();
            for sentence in para.split_inclusive(['.', '!', '?', '\n']) {
                buf.push_str(sentence);
                if buf.len() >= TARGET {
                    chunks.push(buf.trim().to_string());
                    buf.clear();
                    if chunks.len() >= MAX_CHUNKS {
                        break;
                    }
                }
            }
            if !buf.trim().is_empty() && chunks.len() < MAX_CHUNKS {
                current = buf;
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    if !current.trim().is_empty() && chunks.len() < MAX_CHUNKS {
        chunks.push(current.trim().to_string());
    }
    chunks.retain(|c| !c.is_empty());
    chunks
}

/// Ingestion d'un item textuel : upsert + chunks + embeddings (ou NULL si le
/// moteur d'embedding est indisponible → mode dégradé, recherche mot-clé).
pub async fn ingest_item(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    item: Item,
    content: Option<&str>,
) -> Result<String> {
    let title = item.title.clone().unwrap_or_default();
    let source = item.source.clone();
    let (id, changed) = memory::upsert_item(db, &item)?;

    if changed {
        if let Some(text) = content {
            let chunks = chunk(text);
            if !chunks.is_empty() {
                let vectors = match llm.embed(&chunks).await {
                    Ok(vs) => vs
                        .into_iter()
                        .map(|v| Some(vec_to_blob(&v)))
                        .collect::<Vec<_>>(),
                    Err(_) => chunks.iter().map(|_| None).collect(), // embedding en attente
                };
                let rows: Vec<(String, Option<Vec<u8>>)> =
                    chunks.into_iter().zip(vectors.into_iter()).collect();
                memory::replace_embeddings(db, &id, embed_model, &rows)?;
            }
        }
        bus.emit(BusEvent::ItemIngested {
            item_id: id.clone(),
            source,
            title,
        });
    }
    Ok(id)
}

/// Rattrape les embeddings manquants (mode dégradé → nominal), par lots.
pub async fn backfill_embeddings(db: &Db, llm: &Arc<dyn LlmClient>, limit: usize) -> Result<usize> {
    let pending: Vec<(String, String, i64, String)> = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT item_id, model, chunk_index, text FROM embeddings WHERE vector IS NULL LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    if pending.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = pending.iter().map(|(_, _, _, t)| t.clone()).collect();
    let vectors = llm.embed(&texts).await?;
    let n = pending.len().min(vectors.len());
    db.with(|c| {
        let mut stmt = c.prepare(
            "UPDATE embeddings SET vector=?4 WHERE item_id=?1 AND model=?2 AND chunk_index=?3",
        )?;
        for i in 0..n {
            let (id, model, idx, _) = &pending[i];
            stmt.execute(rusqlite::params![id, model, idx, vec_to_blob(&vectors[i])])?;
        }
        Ok(())
    })?;
    Ok(n)
}

/// Extraction d'entités légère et déterministe : noms probables → file d'inconnus.
/// (L'extraction LLM plus riche passe par la boucle, à la demande.)
pub fn extract_entities(db: &Db, item_id: &str, source_ref: &str, text: &str) {
    // Détection d'engagements simples : « je t'envoie X vendredi », « à faire : … »
    let lower = text.to_lowercase();
    for marker in [
        "je t'envoie",
        "je te transmets",
        "je m'occupe de",
        "à faire :",
        "todo:",
    ] {
        if let Some(pos) = lower.find(marker) {
            let end = (pos + 140).min(text.len());
            let snippet: String = text[pos..end]
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(140)
                .collect();
            if snippet.len() > marker.len() + 3 {
                let _ = db.with(|c| {
                    let exists: bool = c
                        .query_row(
                            "SELECT 1 FROM commitments WHERE source_ref=?1 AND text=?2",
                            rusqlite::params![source_ref, snippet],
                            |_| Ok(true),
                        )
                        .unwrap_or(false);
                    if !exists {
                        c.execute(
                            "INSERT INTO commitments (id, text, direction, status, source_ref)
                             VALUES (?1, ?2, 'owed_by_me', 'open', ?3)",
                            rusqlite::params![crate::db::new_id(), snippet, source_ref],
                        )?;
                    }
                    Ok(())
                });
            }
            break;
        }
    }
    let _ = item_id;
}
