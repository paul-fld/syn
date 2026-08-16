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

/// Mots vides français : sans ce filtre, « je recherche un document avec mes
/// quittances » matche tous les documents contenant « avec » ou « mes » et le
/// bruit évince les vrais résultats du LIMIT.
const STOPWORDS_FR: &[&str] = &[
    "les",
    "des",
    "une",
    "aux",
    "ces",
    "mes",
    "tes",
    "ses",
    "nos",
    "vos",
    "leur",
    "leurs",
    "mon",
    "ton",
    "son",
    "que",
    "qui",
    "quoi",
    "dont",
    "mais",
    "donc",
    "car",
    "pour",
    "par",
    "sur",
    "sous",
    "dans",
    "avec",
    "sans",
    "vers",
    "chez",
    "est",
    "sont",
    "suis",
    "etait",
    "ete",
    "etre",
    "avoir",
    "fait",
    "faire",
    "peux",
    "peut",
    "veux",
    "veut",
    "dois",
    "doit",
    "sais",
    "sait",
    "plus",
    "moins",
    "tres",
    "bien",
    "tout",
    "tous",
    "toute",
    "toutes",
    "comme",
    "aussi",
    "alors",
    "ici",
    "cela",
    "cette",
    "celui",
    "celle",
    "ils",
    "elles",
    "nous",
    "vous",
    "moi",
    "toi",
    "lui",
    "elle",
    "ils",
    "quand",
    "comment",
    "pourquoi",
    "quel",
    "quelle",
    "quels",
    "quelles",
    "the",
    "and",
    "was",
    "recherche",
    "cherche",
    "trouve",
    "trouver",
    "retrouve",
    "retrouver",
    "document",
    "documents",
    "fichier",
    "fichiers",
    "dossier",
    "normalement",
    "concernant",
    "lien",
    "lie",
    "lies",
    "pas",
    "parviens",
    "parvient",
    "parvenez",
    "rien",
    "correspond",
    "correspondre",
    "demande",
    "souhaite",
    "souhaitez",
    "information",
    "informations",
    "specifique",
    "traite",
    "traiter",
    "appelle",
    "appele",
    "nomme",
    "range",
    "ranger",
    "donne",
    "donner",
    "cours",
];

/// Radical naïf mais efficace en français : les pluriels réguliers tombent
/// (« quittances » → « quittance »), donc la requête au pluriel matche un
/// contenu au singulier et inversement (LIKE '%quittance%').
fn stem(word: &str) -> String {
    let w = word.to_string();
    if w.chars().count() >= 5 && (w.ends_with('s') || w.ends_with('x')) {
        let mut cs = w.chars();
        cs.next_back();
        cs.as_str().to_string()
    } else {
        w
    }
}

pub(crate) fn keywords(query: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for raw in crate::db::fold(query).split(|c: char| !c.is_alphanumeric()) {
        if raw.chars().count() < 3 || STOPWORDS_FR.contains(&raw) {
            continue;
        }
        let k = stem(raw);
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

fn metadata_keyword_hits(result: &Retrieved, kws: &[String]) -> usize {
    let metadata = crate::db::fold(&format!(
        "{} {}",
        result.title,
        result.path.as_deref().unwrap_or(&result.source_ref)
    ));
    kws.iter()
        .filter(|keyword| metadata.contains(keyword.as_str()))
        .count()
}

fn file_extension(result: &Retrieved) -> String {
    std::path::Path::new(result.path.as_deref().unwrap_or(&result.source_ref))
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_visual_extension(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "heic" | "tiff" | "gif" | "webp" | "bmp"
    )
}

fn is_native_document_extension(extension: &str) -> bool {
    matches!(
        extension,
        "pdf"
            | "doc"
            | "docx"
            | "odt"
            | "rtf"
            | "txt"
            | "md"
            | "xls"
            | "xlsx"
            | "ods"
            | "ppt"
            | "pptx"
            | "key"
            | "pages"
            | "numbers"
    )
}

/// Réordonne les fichiers selon l'intention et la qualité de la preuve.
/// Le nom et le chemin sont généralement plus discriminants que du texte OCR
/// aperçu dans une capture. Le format n'est qu'un a priori : le contenu reste
/// pris en compte et une demande explicite d'image neutralise cette préférence.
fn rerank_files(query: &str, kws: &[String], results: &mut [Retrieved]) {
    let folded_query = crate::db::fold(query);
    let asks_for_visual = [
        "capture",
        "screenshot",
        "photo",
        "image",
        "png",
        "jpg",
        "jpeg",
    ]
    .iter()
    .any(|term| folded_query.contains(term));
    let asks_for_document = !asks_for_visual
        && [
            "document",
            "pdf",
            "word",
            "tableur",
            "presentation",
            "texte",
        ]
        .iter()
        .any(|term| folded_query.contains(term));

    for result in results {
        let metadata_coverage = metadata_keyword_hits(result, kws) as f32 / kws.len().max(1) as f32;
        result.score += 0.45 * metadata_coverage;

        let extension = file_extension(result);
        if asks_for_document {
            if is_native_document_extension(&extension) {
                result.score += 0.35;
            } else if is_visual_extension(&extension) {
                result.score -= 0.40;
            }
        } else if asks_for_visual && is_visual_extension(&extension) {
            result.score += 0.30;
        }
    }
}

/// Recherche hybride sur la mémoire sémantique + structurée.
pub async fn search(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
) -> Result<Vec<Retrieved>> {
    search_scoped(db, llm, query, limit, None).await
}

/// Variante bornée à une source. Le filtrage est effectué dans SQL et dans la
/// recherche vectorielle, avant le LIMIT : filtrer après coup pouvait laisser
/// des documents de projet évincer entièrement les fichiers recherchés.
pub async fn search_source(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
    source: &str,
) -> Result<Vec<Retrieved>> {
    search_scoped(db, llm, query, limit, Some(source)).await
}

async fn search_scoped(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
    source: Option<&str>,
) -> Result<Vec<Retrieved>> {
    let kws = keywords(query);
    let embed_model = crate::settings::load(db)?.embed_model;
    let mut scores: HashMap<String, Retrieved> = HashMap::new();
    // Un résultat sémantique peut paraître proche au modèle tout en étant sans
    // rapport pour l'utilisateur. On conserve donc séparément la preuve
    // lexicale issue du fichier afin de pouvoir l'exiger pour les recherches
    // documentaires explicites.
    let mut lexical_hits: HashMap<String, i64> = HashMap::new();
    let mut semantic_hits: HashMap<String, f32> = HashMap::new();
    let mut item_kinds: HashMap<String, String> = HashMap::new();
    let now = crate::db::now();
    let source_clause = match source {
        Some("files") => " AND i.source='files'",
        Some("mail") => " AND i.source='mail'",
        Some(_) => " AND 0",
        None => "",
    };

    // — Voie structurée (SQL) : mots-clés sur titre/corps/chemin —
    if !kws.is_empty() {
        let like_clauses: Vec<String> = (0..kws.len())
            .map(|i| {
                format!(
                    "(syn_fold(COALESCE(i.title,'')) LIKE '%'||?{0}||'%' OR syn_fold(COALESCE(i.body,'')) LIKE '%'||?{0}||'%' OR syn_fold(COALESCE(i.path,'')) LIKE '%'||?{0}||'%')",
                    i + 1
                )
            })
            .collect();
        let fts_param = kws.len() + 1;
        let sql = format!(
            "SELECT i.id, i.source, i.source_ref, i.title, i.path, substr(COALESCE(i.body, ''), 1, 400), i.mtime,
                    ({}) AS hits
             FROM items i WHERE i.status='active'{source_clause}
               AND i.id IN (
                 SELECT item_id FROM items_fts
                 WHERE items_fts MATCH ?{fts_param}
                 ORDER BY rank LIMIT 500
               )
               AND ({})
             ORDER BY hits DESC LIMIT 80",
            like_clauses
                .iter()
                .map(|c| format!("CASE WHEN {c} THEN 1 ELSE 0 END"))
                .collect::<Vec<_>>()
                .join(" + "),
            like_clauses.join(" OR ")
        );
        db.with(|c| {
            let mut stmt = c.prepare(&sql)?;
            let fts_query = kws
                .iter()
                .map(|keyword| format!("{keyword}*"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut params_vec: Vec<&dyn rusqlite::ToSql> =
                kws.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
            params_vec.push(&fts_query);
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
                let coverage = hits as f32 / kws.len().max(1) as f32;
                let folded_name = crate::db::fold(&format!(
                    "{} {}",
                    title.as_deref().unwrap_or_default(),
                    path.as_deref().unwrap_or_default()
                ));
                let name_hits = kws
                    .iter()
                    .filter(|keyword| folded_name.contains(keyword.as_str()))
                    .count() as f32;
                // La couverture de la demande prime largement sur la récence :
                // une vieille quittance est plus pertinente qu'un README récent.
                let score =
                    0.72 * coverage + 0.18 * (name_hits / kws.len().max(1) as f32) + 0.10 * recency;
                lexical_hits.insert(id.clone(), hits);
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

    // — Voie structurée (entités) : agenda, tâches, engagements, personnes.
    // La doc Intelligence §6.3 l'exige ; sans elle, « mon rendez-vous de mardi »
    // ne peut venir que d'un document.
    if !kws.is_empty() && source.is_none() {
        structured_entities(db, &kws, &mut scores)?;
    }

    // — Voie sémantique (vecteurs) — dégradation gracieuse si embeddings indisponibles.
    if let Ok(qvecs) = llm.embed(&[query.to_string()]).await {
        if let Some(qvec) = qvecs.first() {
            let rows: Vec<(String, String, Vec<u8>)> = db.with(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT e.item_id, e.text, e.vector FROM embeddings e
                     JOIN items i ON i.id = e.item_id
                     WHERE e.vector IS NOT NULL AND e.model = ?1 AND i.status = 'active'{}",
                    source_clause
                ))?;
                let rows =
                    stmt.query_map([&embed_model], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
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
                    String,
                    Option<String>,
                    Option<String>,
                    Option<i64>,
                );
                let metas: Vec<ItemMeta> =
                    db.with(|c| {
                        let mut stmt = c.prepare(
                            "SELECT id, source, source_ref, type, title, path, mtime FROM items WHERE id = ?1",
                        )?;
                        let mut out = vec![];
                        for id in best.keys() {
                            if let Ok(m) = stmt.query_row(params![id], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, String>(3)?,
                                    r.get::<_, Option<String>>(4)?,
                                    r.get::<_, Option<String>>(5)?,
                                    r.get::<_, Option<i64>>(6)?,
                                ))
                            }) {
                                out.push(m);
                            }
                        }
                        Ok(out)
                    })?;
                for (id, source, source_ref, kind, title, path, mtime) in metas {
                    let (sim, text) = best.get(&id).cloned().unwrap_or((0.0, String::new()));
                    if sim < 0.35 {
                        continue; // bruit
                    }
                    semantic_hits.insert(id.clone(), sim);
                    item_kinds.insert(id.clone(), kind);
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
    if source == Some("files") && !kws.is_empty() {
        // Pour une recherche de fichier, on préfère explicitement un faux
        // négatif expliqué à un faux positif absurde. Les embeddings servent à
        // classer les candidats prouvés par le contenu/nom/chemin, jamais à
        // injecter seuls un projet de code sans mot commun avec la demande.
        let min_hits = if kws.len() == 1 {
            1
        } else {
            kws.len().div_ceil(2).max(2)
        } as i64;
        out.retain(|result| {
            lexical_hits
                .get(&result.item_id)
                .is_some_and(|hits| *hits >= min_hits)
                // Un mot métier présent dans le nom ou le dossier constitue
                // une preuve forte, même si le document emploie un synonyme
                // pour le reste de la demande.
                || metadata_keyword_hits(result, &kws) > 0
                // Une forte proximité sémantique couvre les acronymes et les
                // paraphrases, mais uniquement pour les vrais documents : les
                // captures, médias et sources de code ne peuvent pas entrer
                // dans la sélection par cette voie seule.
                || (item_kinds
                    .get(&result.item_id)
                    .is_some_and(|kind| kind == "document")
                    && semantic_hits
                        .get(&result.item_id)
                        .is_some_and(|similarity| *similarity >= 0.72))
        });
        rerank_files(query, &kws, &mut out);
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit.max(1));
    Ok(out)
}

/// Recherche dans les entités structurées (hors items) : les résultats portent
/// un score fixe modeste — un hit exact sur un titre d'événement ou une tâche
/// est presque toujours pertinent.
fn structured_entities(
    db: &Db,
    kws: &[String],
    scores: &mut HashMap<String, Retrieved>,
) -> Result<()> {
    let clause = |cols: &[&str]| -> String {
        (0..kws.len())
            .map(|i| {
                cols.iter()
                    .map(|c| format!("syn_fold(COALESCE({c},'')) LIKE '%'||?{}||'%'", i + 1))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            })
            .map(|c| format!("({c})"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let params_vec: Vec<&dyn rusqlite::ToSql> =
        kws.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    db.with(|c| {
        // Événements d'agenda (miroir natif inclus).
        let sql = format!(
            "SELECT id, title, \"start\", COALESCE(location,''), COALESCE(source_ref, id) FROM events WHERE {} ORDER BY \"start\" DESC LIMIT 6",
            clause(&["title", "location", "notes"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, title, start, location, source_ref) = row?;
            let when = start
                .map(|t| {
                    chrono::DateTime::from_timestamp(t, 0)
                        .map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y %H:%M").to_string())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            scores.entry(format!("event:{id}")).or_insert(Retrieved {
                item_id: format!("event:{id}"),
                source: "calendar".into(),
                source_ref,
                title: title.clone().unwrap_or_default(),
                path: None,
                snippet: format!("Événement d'agenda : {} — {when} {location}", title.unwrap_or_default()),
                score: 0.45,
            });
        }
        // Tâches ouvertes.
        let sql = format!(
            "SELECT id, title, due, status FROM tasks WHERE {} ORDER BY due IS NULL, due LIMIT 6",
            clause(&["title"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, title, due, status) = row?;
            let due_s = due
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|d| format!(" (échéance {})", d.with_timezone(&chrono::Local).format("%d/%m/%Y")))
                .unwrap_or_default();
            scores.entry(format!("task:{id}")).or_insert(Retrieved {
                item_id: format!("task:{id}"),
                source: "tasks".into(),
                source_ref: format!("task:{id}"),
                title: title.clone(),
                path: None,
                snippet: format!("Tâche {status} : {title}{due_s}"),
                score: 0.45,
            });
        }
        // Engagements suivis.
        let sql = format!(
            "SELECT id, text, due, COALESCE(source_ref,'') FROM commitments WHERE {} ORDER BY rowid DESC LIMIT 4",
            clause(&["text"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, text, _due, source_ref) = row?;
            scores.entry(format!("commitment:{id}")).or_insert(Retrieved {
                item_id: format!("commitment:{id}"),
                source: "memory".into(),
                source_ref: if source_ref.is_empty() { format!("commitment:{id}") } else { source_ref },
                title: "Engagement".into(),
                path: None,
                snippet: format!("Engagement suivi : {text}"),
                score: 0.4,
            });
        }
        // Personnes connues (nom ou coordonnées).
        let sql = format!(
            "SELECT id, name, COALESCE(relationship,''), COALESCE(comm_channels,'') FROM people WHERE {} LIMIT 4",
            clause(&["name", "relationship", "comm_channels"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, name, relationship, channels) = row?;
            scores.entry(format!("person:{id}")).or_insert(Retrieved {
                item_id: format!("person:{id}"),
                source: "people".into(),
                source_ref: format!("person:{id}"),
                title: name.clone(),
                path: None,
                snippet: format!(
                    "Personne connue : {name}{} {channels}",
                    if relationship.is_empty() { String::new() } else { format!(" ({relationship})") }
                ),
                score: 0.4,
            });
        }
        Ok(())
    })
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
    assemble_results(results)
}

pub async fn assemble_source(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    source: &str,
) -> Result<ContextBundle> {
    let results = search_source(db, llm, query, TOP_N, source).await?;
    assemble_results(results)
}

fn assemble_results(results: Vec<Retrieved>) -> Result<ContextBundle> {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct NoLlm;
    #[async_trait::async_trait]
    impl LlmClient for NoLlm {
        async fn generate(
            &self,
            _s: &str,
            _m: &[crate::llm::ChatMessage],
            _t: &[crate::llm::ToolSpec],
            _p: crate::llm::GenParams,
        ) -> Result<crate::llm::LlmResponse> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn embed(&self, _t: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn status(&self) -> crate::llm::LlmStatus {
            crate::llm::LlmStatus {
                available: false,
                runtime: "test".into(),
                chat_model_ready: false,
                embed_model_ready: false,
                installed_models: vec![],
                detail: None,
            }
        }
        async fn pull(&self, _m: &str, _p: tokio::sync::mpsc::Sender<(f32, String)>) -> Result<()> {
            Ok(())
        }
    }

    struct MisleadingSemanticLlm;
    #[async_trait::async_trait]
    impl LlmClient for MisleadingSemanticLlm {
        async fn generate(
            &self,
            _s: &str,
            _m: &[crate::llm::ChatMessage],
            _t: &[crate::llm::ToolSpec],
            _p: crate::llm::GenParams,
        ) -> Result<crate::llm::LlmResponse> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        async fn status(&self) -> crate::llm::LlmStatus {
            crate::llm::LlmStatus {
                available: true,
                runtime: "test".into(),
                chat_model_ready: false,
                embed_model_ready: true,
                installed_models: vec![],
                detail: None,
            }
        }
        async fn pull(&self, _m: &str, _p: tokio::sync::mpsc::Sender<(f32, String)>) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn les_mots_vides_et_pluriels_sont_normalises() {
        let kws = keywords("Je recherche un document en lien avec mes quittances de loyer");
        assert!(kws.contains(&"quittance".to_string()), "{kws:?}");
        assert!(kws.contains(&"loyer".to_string()), "{kws:?}");
        assert!(
            !kws.iter()
                .any(|k| k == "avec" || k == "mes" || k == "document"),
            "{kws:?}"
        );
    }

    #[test]
    fn la_normalisation_plie_accents_et_casse() {
        assert_eq!(
            crate::db::fold("Quittance de LOYER décembre"),
            "quittance de loyer decembre"
        );
        assert_eq!(stem("quittances"), "quittance");
        assert_eq!(stem("loyer"), "loyer");
    }

    #[tokio::test]
    async fn retrouve_une_quittance_au_pluriel_comme_au_singulier() {
        let dir = std::env::temp_dir().join(format!("syn-retrieval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('i1','files','/tmp/q.pdf','document','Mail_20251230_Quittance.pdf',
                         'Quittance de loyer — Redevance mensuelle du 01/12/2025',
                         '/Users/x/Documents/Travail/Quittances et factures/Mail_20251230_Quittance.pdf',
                         1, 'active')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(NoLlm);
        // Pluriel dans la requête, singulier dans le document : doit matcher.
        let res = search(&db, &llm, "mes quittances de loyer", 8)
            .await
            .unwrap();
        assert!(!res.is_empty(), "aucun résultat");
        assert_eq!(res[0].item_id, "i1");
        // Et avec du bruit conversationnel autour.
        let res = search(&db, &llm, "je cherche un document en lien avec mes quittances de loyer, tu peux me le retrouver ?", 8)
            .await
            .unwrap();
        assert!(
            !res.is_empty(),
            "le bruit conversationnel évince le résultat"
        );
        assert_eq!(res[0].item_id, "i1");
        // Le filtrage doit avoir lieu avant LIMIT. Quarante résultats mémoire
        // plus forts ne doivent pas faire disparaître l'unique fichier.
        db.with(|c| {
            for n in 0..45 {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1,'conversation',?2,'note','Projet Aberration',
                             'quittance loyer quittance loyer — documentation de projet',1,'active')",
                    params![format!("noise-{n}"), format!("project:{n}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let res = search_source(&db, &llm, "quittance loyer", 8, "files")
            .await
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].item_id, "i1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejette_un_projet_code_meme_si_lembedding_le_classe_premier() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-noise-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('code','files','/tmp/README.md','code','README.md',
                         'Documentation technique du projet Aberration et recette de déploiement.',
                         '/Users/x/Projets/Aberration/README.md',1,'active')",
                [],
            )?;
            c.execute(
                "INSERT INTO embeddings (item_id, model, chunk_index, text, vector)
                 VALUES ('code','test',0,'Projet Aberration',?1)",
                params![crate::llm::vec_to_blob(&[1.0, 0.0])],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(MisleadingSemanticLlm);
        let results = search_source(
            &db,
            &llm,
            "Retrouve un document lié à ma quittance de loyer",
            8,
            "files",
        )
        .await
        .unwrap();
        assert!(
            results.is_empty(),
            "un embedding seul ne doit jamais faire remonter {results:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn un_document_natif_prime_sur_des_captures_ocr_plus_recentes() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-ranking-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, mtime, ingested_at, status)
                 VALUES ('document','files','/tmp/Archives/Mail_2025_Quittance.pdf','document',
                         'Mail_2025_Quittance.pdf','Quittance de loyer — période mensuelle',
                         '/tmp/Archives/Quittances et factures/Mail_2025_Quittance.pdf',1,1,'active')",
                [],
            )?;
            for n in 0..8 {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, path, mtime, ingested_at, status)
                     VALUES (?1,'files',?2,'photo',?3,
                             'Capture d’une conversation : recherche de quittance de loyer',
                             ?2,9999999999,1,'active')",
                    params![
                        format!("capture-{n}"),
                        format!("/tmp/Capture écran {n}.png"),
                        format!("Capture écran {n}.png")
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(NoLlm);
        let results = search_source(
            &db,
            &llm,
            "Retrouve un document lié à ma quittance de loyer",
            8,
            "files",
        )
        .await
        .unwrap();
        assert_eq!(results[0].item_id, "document", "{results:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn retrouve_un_document_conceptuel_sans_nom_ni_emplacement_connus() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-concept-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('policy','files','/tmp/Interne/Reference_2025.pdf','document',
                         'Reference_2025.pdf',
                         'Politique de sécurité des systèmes d’information applicable au personnel',
                         '/tmp/Interne/Reference_2025.pdf',1,'active')",
                [],
            )?;
            c.execute(
                "INSERT INTO embeddings (item_id, model, chunk_index, text, vector)
                 VALUES ('policy','test',0,
                         'Politique de sécurité des systèmes d’information applicable au personnel',?1)",
                params![crate::llm::vec_to_blob(&[1.0, 0.0])],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(MisleadingSemanticLlm);
        let results = search_source(
            &db,
            &llm,
            "Peux-tu retrouver le document sur la PSSI de mon entreprise ?",
            8,
            "files",
        )
        .await
        .unwrap();
        assert_eq!(results[0].item_id, "policy", "{results:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
