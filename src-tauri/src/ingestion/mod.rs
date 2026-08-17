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
        let chunks = content.map(chunk).unwrap_or_default();
        // Barrière de disponibilité : FTS/BM25 reçoit le texte extrait et ses
        // chunks AVANT tout appel réseau/modèle. Une recherche concurrente ne
        // peut donc jamais attendre l'embedding.
        let lexical_rows = chunks
            .iter()
            .cloned()
            .map(|text| (text, None))
            .collect::<Vec<_>>();
        memory::replace_embeddings(db, &id, embed_model, &lexical_rows)?;
        bus.emit(BusEvent::ItemIngested {
            item_id: id.clone(),
            source,
            title,
        });
        if !chunks.is_empty() {
            if let Ok(vectors) = llm.embed(&chunks).await {
                let rows = chunks
                    .into_iter()
                    .zip(vectors.into_iter().map(|vector| Some(vec_to_blob(&vector))))
                    .collect::<Vec<_>>();
                memory::replace_embeddings(db, &id, embed_model, &rows)?;
            }
        }
    }
    Ok(id)
}

/// Rattrape les embeddings manquants (mode dégradé → nominal), par lots.
pub async fn backfill_embeddings(db: &Db, llm: &Arc<dyn LlmClient>, limit: usize) -> Result<usize> {
    let embed_model = crate::settings::load(db)?.embed_model;
    let pending: Vec<(String, String, i64, String)> = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT item_id, model, chunk_index, text FROM embeddings
             WHERE vector IS NULL AND model=?2 LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64, embed_model], |r| {
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
        // Le rattrapage ne vectorise que ce qui est DÉJÀ en base. Pour un objet
        // cloud, ce « déjà » se limite aux métadonnées : le corps du document
        // n'est téléchargé que par `external::enrich_item`, via la file. Le
        // marquer « embedded » ici le sortirait de la file et son contenu ne
        // serait jamais indexé — c'est ce qui rendait Drive et OneDrive muets.
        let mut done = c.prepare(
            "UPDATE enrichment_queue SET
                 state=CASE WHEN source='cloud' AND state IN ('pending','error')
                            THEN state ELSE 'embedded' END,
                 embedding_ready=1,lexical_ready=1,updated_at=?2
             WHERE item_id=?1",
        )?;
        for (id, _, _, _) in &pending[..n] {
            done.execute(rusqlite::params![id, crate::db::now()])?;
        }
        Ok(())
    })?;
    Ok(n)
}
