//! La boucle agentique (Intelligence §5) : perceive → retrieve → plan → act →
//! observe → respond, orchestrée en Rust. Le modèle n'orchestre pas — il est
//! appelé À L'INTÉRIEUR de la boucle. La confirmation est un point d'arrêt
//! DANS la boucle (plancher humain), pas une couche UI par-dessus.

pub mod prompt;

use crate::actions;
use crate::bus::BusEvent;
use crate::error::Result;
use crate::llm::{ChatMessage, GenParams};
use crate::memory;
use crate::retrieval;
use crate::security::provenance;
use crate::state::Core;
use serde::Serialize;
use serde_json::{json, Value};

const MAX_TOOL_ITERATIONS: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub text: String,
    pub sources: Vec<retrieval::Retrieved>,
    pub pending_actions: Vec<PendingRef>,
    pub session_id: String,
    pub degraded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingRef {
    pub action_id: String,
    pub tool: String,
    pub preview: String,
    pub risk_class: String,
}

pub async fn handle_query(core: &Core, session_id: &str, user_text: &str) -> Result<Answer> {
    handle_query_with_context(core, session_id, user_text, None).await
}

pub async fn handle_query_with_context(
    core: &Core,
    session_id: &str,
    user_text: &str,
    screen_context: Option<&Value>,
) -> Result<Answer> {
    let db = &core.db;
    let settings = crate::settings::load(db)?;

    emit_progress(
        core,
        session_id,
        "perceive",
        "Demande reçue",
        None,
        1,
        5,
        "running",
    );
    // 1. PERCEVOIR — continuité de conversation.
    memory::ensure_session(db, session_id, user_text)?;
    memory::persist_turn(db, session_id, "user", user_text)?;
    if let Some(answer) =
        confirm_pending_mail_from_chat(core, session_id, user_text, &settings).await?
    {
        return Ok(answer);
    }
    let convo = memory::recent_turns(db, session_id, 12)?;
    // Copie de la parole utilisateur avant ajout éventuel du contexte d'écran :
    // elle sert à vérifier qu'un destinataire vient bien d'un canal fiable.
    let trusted_user_history = convo
        .iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let device_only = is_device_diagnostic_query(user_text);
    let file_request = resolve_file_search_request(user_text, &convo);
    let file_search = file_request.is_some();
    let mail_composition = is_mail_composition_query(user_text)
        || (is_mail_content_followup(user_text)
            && trusted_user_history.lines().any(is_mail_composition_query));
    emit_progress(
        core,
        session_id,
        "retrieve",
        if device_only {
            "Lecture directe de l’appareil"
        } else {
            "Recherche du contexte local"
        },
        Some(if device_only {
            "Métriques système actuelles, sans recherche documentaire".into()
        } else {
            "Documents, conversations et données autorisées".into()
        }),
        2,
        5,
        "running",
    );
    // La recherche documentaire est un parcours produit déterministe. Le
    // modèle n'a pas à décider s'il doit chercher, ni à reformuler librement
    // une liste de résultats : c'est précisément ce qui produisait des README
    // et des fichiers de code hors sujet, puis une réponse générique en anglais.
    if let Some((query, is_correction)) = file_request {
        return answer_file_search(
            core,
            session_id,
            &query,
            is_correction,
            settings.voice.formality == "vous",
        )
        .await;
    }
    if mail_composition && mail_request_missing_content(user_text) {
        let text = if settings.voice.formality == "vous" {
            "Que voulez-vous dire dans ce mail ? Je n’ai encore préparé ni envoyé aucun message."
        } else {
            "Que veux-tu dire dans ce mail ? Je n’ai encore préparé ni envoyé aucun message."
        }
        .to_string();
        memory::persist_turn(db, session_id, "assistant", &text)?;
        emit_progress(
            core,
            session_id,
            "clarify",
            "Contenu du mail nécessaire",
            None,
            5,
            5,
            "waiting",
        );
        return Ok(Answer {
            text,
            sources: vec![],
            pending_actions: vec![],
            session_id: session_id.into(),
            degraded: false,
        });
    }
    // 2. RÉCUPÉRER — retrieval hybride borné et sourcé.
    let mut ctx = if device_only || mail_composition {
        retrieval::ContextBundle {
            fragments: vec![],
            sources: vec![],
            untrusted_text: String::new(),
        }
    } else if file_search {
        retrieval::assemble_source(db, &core.llm, user_text, "files").await?
    } else {
        retrieval::assemble(db, &core.llm, user_text).await?
    };

    // Une recherche de fichier ou une composition de mail est une tâche ciblée.
    // La mémoire d'un projet attaché à la conversation ne doit jamais se
    // substituer au connecteur demandé (cas « quittance » → projet Aberration).
    if !file_search && !mail_composition {
        if let Some((project_id, project_name, history)) =
            memory::project_context(db, session_id, 24)?
        {
            let citation = ctx.sources.len() + 1;
            let source_ref = format!("project:{project_id}");
            let contextualized = format!(
                "[source:{citation}] Mémoire partagée du projet « {project_name} »\n{history}"
            );
            ctx.fragments.push((
                citation,
                provenance::wrap_untrusted(&source_ref, &contextualized),
            ));
            ctx.untrusted_text.push_str(&history);
            ctx.sources.push(retrieval::Retrieved {
                item_id: source_ref.clone(),
                source: "project".into(),
                source_ref,
                title: format!("Projet — {project_name}"),
                path: None,
                snippet: history.chars().take(500).collect(),
                score: 1.0,
            });
        }
    }

    // Règles actives injectées dans le comportement.
    let (style_rules, action_modifiers) = crate::rules::active_rule_texts(db)?;
    let mut system =
        prompt::build_system(&settings, &style_rules, &action_modifiers, &ctx.fragments);
    if file_search {
        system.push_str("\nLa demande actuelle est explicitement une recherche de FICHIER. Utilise files.search et ne réponds pas à partir d'une mémoire de projet ou d'une documentation sans rapport.\n");
    }
    // Mémoire de travail longue : les tours anciens condensés (doc §13).
    if let Ok(Some(summary)) = memory::session_summary(db, session_id) {
        system.push_str(&format!(
            "\n— Mémoire de la conversation (résumé des échanges antérieurs, déjà validé) —\n{summary}\n"
        ));
    }

    let mut messages: Vec<ChatMessage> = convo
        .into_iter()
        .map(|(role, content)| ChatMessage {
            role,
            content,
            tool_calls: None,
            tool_name: None,
        })
        .collect();

    // Le message conservé dans l'historique reste la demande de l'utilisateur.
    // La capture n'est jointe qu'au tour envoyé au modèle, sous marqueurs non fiables.
    let screen_text = screen_context.and_then(screen_context_text);
    if let Some(observed) = &screen_text {
        if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
            last_user.content.push_str("\n\n");
            last_user.content.push_str(&provenance::wrap_untrusted(
                "screen:capture_locale",
                observed,
            ));
        }
    }

    let catalog = crate::tools::catalog();
    let tool_ctx = crate::tools::ToolCtx {
        db: db.clone(),
        llm: core.llm.clone(),
        bus: core.bus.clone(),
        settings: settings.clone(),
    };

    let mut pending: Vec<PendingRef> = vec![];
    let mut final_text = String::new();
    let mut degraded = false;

    // 3–4. PLANIFIER / AGIR / OBSERVER — boucle multi-tours d'outils, bornée.
    for iteration in 0..MAX_TOOL_ITERATIONS {
        emit_progress(
            core,
            session_id,
            "plan",
            "Préparation de la réponse",
            Some(format!(
                "Étape agentique {} sur {}",
                iteration + 1,
                MAX_TOOL_ITERATIONS
            )),
            3,
            5,
            "running",
        );
        let resp = match core
            .llm
            .generate(
                &system,
                &messages,
                &catalog,
                GenParams {
                    temperature: 0.3,
                    max_tokens: Some(1200),
                    json: false,
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Mode dégradé (doc maître §22) : le retrieval fonctionne,
                // la génération est signalée indisponible.
                degraded = true;
                final_text = degraded_answer(&ctx, &e.to_string());
                break;
            }
        };

        if resp.tool_calls.is_empty() {
            final_text = resp.content;
            break;
        }

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: resp.content.clone(),
            tool_calls: Some(resp.tool_calls.clone()),
            tool_name: None,
        });

        for call in &resp.tool_calls {
            emit_progress(
                core,
                session_id,
                "tool",
                &progress_title(&call.name),
                Some(crate::tools::preview_for(&call.name, &call.arguments)),
                4,
                5,
                "running",
            );
            let args_text = call.arguments.to_string();
            let risk = actions::classify(&call.name, &call.arguments);
            let all_untrusted = match &screen_text {
                Some(screen) => format!("{}\n{}", ctx.untrusted_text, screen),
                None => ctx.untrusted_text.clone(),
            };
            let untrusted =
                provenance::args_derived_from_untrusted(&args_text, user_text, &all_untrusted);

            let mail_preflight = if call.name == "mail.send" {
                mail_send_preflight(db, &call.arguments, user_text, &trusted_user_history)
            } else {
                None
            };
            let mut verified_arguments = call.arguments.clone();
            if call.name == "mail.send" && mail_preflight.is_none() {
                // Marqueur interne ajouté uniquement après les contrôles ci-dessus.
                // Les anciennes actions en attente (créées avant ce correctif)
                // ne peuvent ainsi pas envoyer une adresse inventée au clic.
                verified_arguments["_syn_preflight_v1"] = json!(true);
            }

            let observation = if let Some(reason) = mail_preflight {
                reason
            } else if untrusted
                && risk != actions::RiskClass::Read
                && !explicit_action_intent(user_text, &call.name)
            {
                json!({
                    "status": "refuse_provenance",
                    "note": "Action ignorée : elle semble provenir d'un document ou d'un contenu observé, pas d'une demande explicite de l'utilisateur."
                })
            } else if actions::needs_confirmation(risk, &settings.autonomy, untrusted, &call.name) {
                // Point d'arrêt : plancher / seuil d'autonomie.
                let preview = crate::tools::preview_for(&call.name, &verified_arguments);
                let action_id = actions::queue_pending(
                    db,
                    &call.name,
                    &verified_arguments,
                    risk,
                    &preview,
                    untrusted,
                    Some(session_id),
                )?;
                core.bus.emit(BusEvent::ActionAwaitingConfirmation {
                    action_id: action_id.clone(),
                    tool: call.name.clone(),
                    preview: preview.clone(),
                    risk_class: risk.as_str().into(),
                });
                pending.push(PendingRef {
                    action_id: action_id.clone(),
                    tool: call.name.clone(),
                    preview,
                    risk_class: risk.as_str().into(),
                });
                json!({"status": "en_attente_de_confirmation", "action_id": action_id,
                       "note": "L'utilisateur doit confirmer cette action avant exécution."})
            } else {
                match crate::tools::execute(&tool_ctx, &call.name, &verified_arguments).await {
                    Ok(outcome) => {
                        if risk != actions::RiskClass::Read {
                            let preview = crate::tools::preview_for(&call.name, &call.arguments);
                            actions::log_executed(
                                db,
                                &call.name,
                                &call.arguments,
                                risk,
                                &preview,
                                &outcome
                                    .result
                                    .to_string()
                                    .chars()
                                    .take(500)
                                    .collect::<String>(),
                                outcome.undo.as_ref(),
                                untrusted,
                            )?;
                        }
                        let mut result = outcome.result;
                        // Un plan de rangement est toujours suivi d'une carte de validation
                        // déterministe. Le modèle ne doit pas inventer ni reperdre son plan_id.
                        if call.name == "files.reorganize"
                            && explicit_action_intent(user_text, "files.apply_reorganize_plan")
                        {
                            if let Some(plan_id) = result["plan_id"].as_str() {
                                // La carte de confirmation doit montrer le plan
                                // complet avant que l'utilisateur accepte. Le
                                // moteur conserve aussi sa copie en base via
                                // plan_id pour ne jamais exécuter une donnée UI.
                                let apply_args = json!({
                                    "plan_id": plan_id,
                                    "plan": result["plan"].clone()
                                });
                                let apply_risk =
                                    actions::classify("files.apply_reorganize_plan", &apply_args);
                                let summary = result["plan"]["summary"]
                                    .as_str()
                                    .unwrap_or("Plan de rangement prêt");
                                let preview = format!("Valider le rangement — {summary}");
                                let action_id = actions::queue_pending(
                                    db,
                                    "files.apply_reorganize_plan",
                                    &apply_args,
                                    apply_risk,
                                    &preview,
                                    false,
                                    Some(session_id),
                                )?;
                                core.bus.emit(BusEvent::ActionAwaitingConfirmation {
                                    action_id: action_id.clone(),
                                    tool: "files.apply_reorganize_plan".into(),
                                    preview: preview.clone(),
                                    risk_class: apply_risk.as_str().into(),
                                });
                                pending.push(PendingRef {
                                    action_id: action_id.clone(),
                                    tool: "files.apply_reorganize_plan".into(),
                                    preview,
                                    risk_class: apply_risk.as_str().into(),
                                });
                                result["confirmation_action_id"] = json!(action_id);
                                result["status"] = json!("en_attente_de_confirmation");
                            }
                        }
                        emit_progress(
                            core,
                            session_id,
                            "observe",
                            "Résultat de l’action vérifié",
                            Some(progress_title(&call.name)),
                            4,
                            5,
                            "done",
                        );
                        result
                    }
                    Err(e) => {
                        emit_progress(
                            core,
                            session_id,
                            "error",
                            "Étape interrompue",
                            Some(e.to_string()),
                            4,
                            5,
                            "error",
                        );
                        json!({"error": e.to_string()})
                    }
                }
            };

            // Sortie d'outil = donnée non fiable réinjectée comme observation.
            let obs_text = observation.to_string();
            let obs_capped: String = obs_text.chars().take(4000).collect();
            messages.push(ChatMessage::tool(&call.name, obs_capped));
        }

        // Ne remplace la réponse par le message de limite QUE si le modèle n'a
        // rien produit : écraser un texte final valide était un bug (audit §3).
        if iteration == MAX_TOOL_ITERATIONS - 1 && final_text.trim().is_empty() {
            final_text = "J'ai atteint ma limite d'étapes pour cette demande — voici où j'en suis. Reprécise si besoin.".into();
        }
    }

    if final_text.trim().is_empty() {
        final_text = if pending.is_empty() {
            "Je n'ai pas réussi à formuler de réponse. Réessaie en reformulant.".into()
        } else {
            "L'action préparée attend ta validation.".into()
        };
    }

    // La recherche peut examiner plusieurs fragments sans que la réponse les utilise.
    // On ne surface que les sources effectivement citées et on renumérote les
    // citations pour garder les pastilles et le texte parfaitement alignés.
    let (normalized_text, cited_sources) = cited_sources(&final_text, &ctx.sources);
    final_text = normalized_text;

    // 5. RÉPONDRE — persisté, sourcé.
    memory::persist_turn(db, session_id, "assistant", &final_text)?;

    // Mémoire longue : au-delà de la fenêtre récente, condenser les tours
    // anciens en arrière-plan (best-effort, jamais bloquant pour la réponse).
    if let Ok(count) = memory::turn_count(db, session_id) {
        if count >= 18 && count % 6 == 0 {
            let db2 = db.clone();
            let llm2 = core.llm.clone();
            let sid = session_id.to_string();
            tauri::async_runtime::spawn(async move {
                let _ = summarize_session(&db2, &llm2, &sid).await;
            });
        }
    }
    emit_progress(
        core,
        session_id,
        if pending.is_empty() {
            "complete"
        } else {
            "confirm"
        },
        if pending.is_empty() {
            "Réponse terminée"
        } else {
            "Validation utilisateur requise"
        },
        None,
        5,
        5,
        if pending.is_empty() {
            "done"
        } else {
            "waiting"
        },
    );
    Ok(Answer {
        text: final_text,
        sources: cited_sources,
        pending_actions: pending,
        session_id: session_id.to_string(),
        degraded,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_progress(
    core: &Core,
    session_id: &str,
    stage: &str,
    title: &str,
    detail: Option<String>,
    current: u32,
    total: u32,
    status: &str,
) {
    core.bus.emit(BusEvent::AgentProgress {
        session_id: session_id.into(),
        stage: stage.into(),
        title: title.into(),
        detail,
        current,
        total,
        status: status.into(),
    });
}

fn progress_title(tool: &str) -> String {
    match tool {
        "memory.query" | "files.search" => "Recherche dans les données autorisées".into(),
        "files.reorganize" => "Analyse et classement du dossier".into(),
        "files.move" => "Résolution et déplacement de l’élément demandé".into(),
        "files.create_folder_and_move" => "Création du dossier et rangement du document".into(),
        "files.apply_reorganize_plan" => "Déplacement des éléments validés".into(),
        "mail.search" => "Recherche dans les messages autorisés".into(),
        "calendar.list" => "Lecture du calendrier autorisé".into(),
        "system.diagnose" => "Diagnostic de l’appareil".into(),
        _ => format!("Exécution de {tool}"),
    }
}

fn screen_context_text(context: &Value) -> Option<String> {
    if context["available"].as_bool() != Some(true) {
        return None;
    }
    let app = context["app"].as_str().unwrap_or("").trim();
    let window = context["window"].as_str().unwrap_or("").trim();
    let text = context["text"].as_str().unwrap_or("").trim();
    let mut out = if app.is_empty() {
        "Capture ponctuelle de l’écran. L’application cible n’a pas pu être identifiée : ne suppose pas qu’il s’agit de Syn."
            .to_string()
    } else {
        format!(
            "Capture ponctuelle de l’écran. Application cible déterminée par macOS après exclusion des fenêtres de Syn : {app}. Cette identité est prioritaire sur les mots reconnus dans la capture : la présence du mot « Syn » dans le contenu ne signifie pas que l’application affichée est Syn."
        )
    };
    if !window.is_empty() {
        out.push_str(&format!(" Fenêtre : {window}."));
    }
    out.push_str(" Les préfixes entre crochets indiquent la zone visuelle approximative. Décris uniquement ce que ces observations attestent et présente toute interprétation comme une hypothèse.\n");
    out.extend(text.chars().take(16_000));
    Some(out)
}

fn is_device_diagnostic_query(text: &str) -> bool {
    let text = text.to_lowercase();
    let device = [
        "mon mac",
        "mon ordinateur",
        "ma machine",
        "mon appareil",
        "ce mac",
        "cpu",
        "processeur",
        "mémoire vive",
        "memoire vive",
        " ram",
        "batterie",
        "température",
        "temperature",
        "stockage",
        "espace disque",
        "disque dur",
    ]
    .iter()
    .any(|term| text.contains(term));
    let diagnostic = [
        "métrique",
        "metrique",
        "capacité",
        "capacite",
        "état",
        "etat",
        "santé",
        "sante",
        "chauff",
        "température",
        "temperature",
        "utilisation",
        "charge",
        "disponible",
        "combien",
        "diagnostic",
        "performance",
    ]
    .iter()
    .any(|term| text.contains(term));
    let asks_documents = [
        "dans mes documents",
        "dans mes fichiers",
        "document sur",
        "fichier sur",
        "note sur",
        "pdf sur",
    ]
    .iter()
    .any(|term| text.contains(term));
    device && diagnostic && !asks_documents
}

fn is_file_search_query(text: &str) -> bool {
    let text = crate::db::fold(text);
    let asks_search = [
        "cherche",
        "recherche",
        "trouve",
        "retrouve",
        "retrouver",
        "ou est",
        "localise",
    ]
    .iter()
    .any(|term| text.contains(term));
    let asks_file = [
        "document",
        "fichier",
        "pdf",
        "piece jointe",
        "quittance",
        "facture",
        "contrat",
        "bail",
        "cours",
        "rapport",
        "presentation",
        "tableur",
        "note",
    ]
    .iter()
    .any(|term| text.contains(term));
    asks_search && asks_file
}

fn is_file_search_correction(text: &str) -> bool {
    let text = crate::db::fold(text);
    let refers_to_results = [
        "ces fichiers",
        "ces documents",
        "les fichiers",
        "les documents",
        "les resultats",
        "ce resultat",
        "cette liste",
        "certains fichiers",
        "certains des fichiers",
    ]
    .iter()
    .any(|term| text.contains(term));
    let rejects = [
        "rien a voir",
        "hors sujet",
        "a cote de la plaque",
        "pas le bon",
        "pas les bons",
        "incorrect",
        "mauvais",
        "aucun rapport",
        "pas dans cette liste",
        "ne figure pas",
        "introuvable dans",
    ]
    .iter()
    .any(|term| text.contains(term));
    refers_to_results && rejects
}

/// Conserve l'intention de recherche lors d'une correction naturelle telle
/// que « ces fichiers n'ont rien à voir ». Le dernier tour utilisateur est le
/// texte courant, déjà persisté avant cet appel ; on remonte donc au précédent.
fn resolve_file_search_request(
    current: &str,
    conversation: &[(String, String)],
) -> Option<(String, bool)> {
    // Une correction peut elle-même contenir « document » et « cherche ».
    // Elle doit donc être reconnue avant une nouvelle demande autonome, sinon
    // Syn recherche les mots du reproche (« liste », « fichiers »…) et perd
    // complètement le sujet précédent.
    if is_file_search_correction(current) {
        let mut current_skipped = false;
        for (role, content) in conversation.iter().rev() {
            if role != "user" {
                continue;
            }
            if !current_skipped {
                current_skipped = true;
                continue;
            }
            if is_file_search_query(content) {
                return Some((content.clone(), true));
            }
        }
        return None;
    }
    if is_file_search_query(current) {
        return Some((current.to_string(), false));
    }
    None
}

fn file_search_variants(query: &str) -> Vec<String> {
    let folded = crate::db::fold(query);
    if folded.contains("quittance") || folded.contains("loyer") {
        return vec!["quittance".into(), "loyer".into(), "redevance".into()];
    }
    Vec::new()
}

async fn answer_file_search(
    core: &Core,
    session_id: &str,
    query: &str,
    is_correction: bool,
    formal: bool,
) -> Result<Answer> {
    let mut results = retrieval::search_source(&core.db, &core.llm, query, 8, "files").await?;
    filter_file_domain(query, &mut results);

    // Filet de sécurité immédiat pendant la construction de l'index : cherche
    // les noms et dossiers directement sur le périmètre autorisé, puis programme
    // leur ingestion. Aucun chemin utilisateur ni cas métier n'est codé ici.
    let roots = crate::connectors::files::folder_paths(&core.db)?;
    let keywords = retrieval::keywords(query);
    let mut live_results = tokio::task::spawn_blocking(move || {
        crate::connectors::files::live_metadata_search(&roots, &keywords, 12)
    })
    .await
    .unwrap_or_default();
    filter_file_domain(query, &mut live_results);
    if !live_results.is_empty() {
        let live_paths = live_results
            .iter()
            .map(|result| std::path::PathBuf::from(&result.source_ref))
            .collect();
        let _ = core
            .indexer
            .tx
            .send(crate::connectors::files::IndexJob::Paths(live_paths));
        // Recherche en niveaux : une correspondance explicite dans le nom ou
        // le dossier est une preuve plus forte qu'une occurrence aperçue dans
        // le contenu ou l'OCR d'une capture. Mélanger les deux niveaux recréait
        // précisément le bruit que l'utilisateur cherchait à éviter.
        results = live_results;
    }

    // Deuxième passe explicable uniquement si la formulation d'origine ne
    // donne rien. Les variantes sont un petit dictionnaire métier contrôlé,
    // pas une divagation générée par le LLM.
    if results.is_empty() {
        let mut by_id = std::collections::HashMap::new();
        for variant in file_search_variants(query) {
            for result in
                retrieval::search_source(&core.db, &core.llm, &variant, 4, "files").await?
            {
                if !file_matches_requested_domain(query, &result) {
                    continue;
                }
                by_id
                    .entry(result.item_id.clone())
                    .and_modify(|old: &mut retrieval::Retrieved| {
                        if result.score > old.score {
                            *old = result.clone();
                        }
                    })
                    .or_insert(result);
            }
        }
        results = by_id.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(6);
    }

    crate::security::log_access(&core.db, "files", "search", Some(query));
    let status = core.indexer.status(&core.db)?;
    let correction_prefix = if is_correction {
        if formal {
            "Vous avez raison : les résultats précédents étaient hors sujet. Je les ai écartés.\n\n"
        } else {
            "Tu as raison : les résultats précédents étaient hors sujet. Je les ai écartés.\n\n"
        }
    } else {
        ""
    };

    let mut text = if results.is_empty() {
        let state = if status.items_count == 0 {
            "L’index de fichiers est vide : Syn ne peut pas encore vérifier le contenu du disque."
                .to_string()
        } else if status.running || status.pending_embeddings > 0 {
            format!(
                "L’indexation est encore en cours ({} fichiers déjà indexés, {} analyses sémantiques en attente).",
                status.items_count, status.pending_embeddings
            )
        } else {
            format!(
                "Aucun fichier suffisamment pertinent n’a été trouvé parmi les {} fichiers indexés (dont {} sans texte extractible).",
                status.items_count, status.unreadable_files
            )
        };
        format!(
            "{correction_prefix}{state} J’ai volontairement supprimé les résultats sans rapport au lieu de vous présenter des fichiers au hasard. Le document peut employer un autre vocabulaire, être une image sans texte exploitable, ou ne pas avoir encore été parcouru."
        )
        .replace("vous présenter", if formal { "vous présenter" } else { "te présenter" })
    } else {
        let mut body = format!(
            "{correction_prefix}J’ai trouvé {} fichier{} avec une correspondance vérifiable :\n",
            results.len(),
            if results.len() > 1 { "s" } else { "" }
        );
        for (index, result) in results.iter().enumerate() {
            let location = result.path.as_deref().unwrap_or(&result.source_ref);
            body.push_str(&format!(
                "\n{}. **{}** — {}",
                index + 1,
                result.title,
                location
            ));
        }
        body.push_str(if formal {
            "\n\nCliquez sur le nom d’un document pour l’ouvrir."
        } else {
            "\n\nClique sur le nom d’un document pour l’ouvrir."
        });
        body
    };

    let mut pending_actions = Vec::new();
    if let Some((initiative, pending)) =
        location_initiative(core, session_id, query, &results, formal)?
    {
        text.push_str("\n\n");
        text.push_str(&initiative);
        pending_actions.push(pending);
    }

    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "complete",
        if results.is_empty() {
            "Recherche terminée sans résultat fiable"
        } else {
            "Fichiers pertinents trouvés"
        },
        Some(format!("{} fichiers indexés", status.items_count)),
        5,
        5,
        "done",
    );
    Ok(Answer {
        text,
        sources: results,
        pending_actions,
        session_id: session_id.into(),
        degraded: false,
    })
}

#[derive(Debug, Clone, PartialEq)]
struct ExpectedFolder {
    path: std::path::PathBuf,
    label: String,
}

fn expected_folder_from_query(query: &str, home: &std::path::Path) -> Option<ExpectedFolder> {
    let folded = crate::db::fold(query);
    let bases = [
        (
            &["dans les documents", "dans mes documents", "sous documents"][..],
            "Documents",
        ),
        (
            &[
                "sur mon bureau",
                "sur le bureau",
                "dans desktop",
                "sur desktop",
            ][..],
            "Desktop",
        ),
        (
            &[
                "dans les telechargements",
                "dans mes telechargements",
                "dans downloads",
            ][..],
            "Downloads",
        ),
        (
            &["dans mes images", "dans les images", "dans pictures"][..],
            "Pictures",
        ),
    ];
    let base_name = bases
        .iter()
        .find(|(markers, _)| markers.iter().any(|marker| folded.contains(marker)))
        .map(|(_, name)| *name)?;
    let base = home.join(base_name);

    // « dans un dossier RH dans mes Documents » : extrait seulement le nom
    // fourni par l'utilisateur, sans déduire une catégorie métier.
    let lower = query.to_lowercase();
    let child = lower.find("dossier").and_then(|position| {
        let mut tail = query[position + "dossier".len()..].trim_start();
        for article in ["un ", "une ", "le ", "la ", "mon ", "ma ", "mes "] {
            if tail.to_lowercase().starts_with(article) {
                tail = tail[article.len()..].trim_start();
                break;
            }
        }
        let folded_tail = crate::db::fold(tail);
        let end = [" dans ", " sous ", ",", ".", "?", ";"]
            .iter()
            .filter_map(|separator| folded_tail.find(separator))
            .min()
            .unwrap_or(tail.len());
        let name = tail[..end]
            .trim()
            .trim_matches(|character| matches!(character, '«' | '»' | '\'' | '"'));
        (!name.is_empty() && name.len() <= 80 && !name.contains('/') && !name.contains('\\'))
            .then(|| name.to_string())
    });
    let path = child
        .as_deref()
        .map_or_else(|| base.clone(), |name| base.join(name));
    let label = path
        .strip_prefix(home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string());
    Some(ExpectedFolder { path, label })
}

fn friendly_location(path: &std::path::Path, home: &std::path::Path) -> String {
    path.strip_prefix(home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn location_initiative(
    core: &Core,
    session_id: &str,
    query: &str,
    results: &[retrieval::Retrieved],
    formal: bool,
) -> Result<Option<(String, PendingRef)>> {
    // Une initiative de rangement exige un résultat non ambigu.
    let [result] = results else {
        return Ok(None);
    };
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => return Ok(None),
    };
    let expected = match expected_folder_from_query(query, &home) {
        Some(expected) => expected,
        None => return Ok(None),
    };
    let source = std::path::Path::new(result.path.as_deref().unwrap_or(&result.source_ref));
    if !source.is_file() || source.starts_with(&expected.path) {
        return Ok(None);
    }
    if expected.path.exists() && !expected.path.is_dir() {
        return Ok(None);
    }
    let actual = source.parent().unwrap_or(source);
    let actual_label = friendly_location(actual, &home);
    let creates_folder = !expected.path.exists();
    let tool = if creates_folder {
        "files.create_folder_and_move"
    } else {
        "files.move"
    };
    let args = json!({
        "source": source.to_string_lossy(),
        "destination": expected.path.to_string_lossy(),
    });
    let risk = actions::classify(tool, &args);
    let preview = if creates_folder {
        format!(
            "Créer « {} » et y ranger « {} »",
            expected.label, result.title
        )
    } else {
        format!("Ranger « {} » dans « {} »", result.title, expected.label)
    };
    let action_id = actions::queue_pending(
        &core.db,
        tool,
        &args,
        risk,
        &preview,
        false,
        Some(session_id),
    )?;
    core.bus.emit(BusEvent::ActionAwaitingConfirmation {
        action_id: action_id.clone(),
        tool: tool.into(),
        preview: preview.clone(),
        risk_class: risk.as_str().into(),
    });
    let initiative = if creates_folder {
        if formal {
            format!(
                "En revanche, il se trouvait dans « {actual_label} », et non dans « {} ». Ce dossier n’existe pas encore. Souhaitez-vous que je le crée et que j’y range ce document ?",
                expected.label
            )
        } else {
            format!(
                "En revanche, il se trouvait dans « {actual_label} », et non dans « {} ». Ce dossier n’existe pas encore. Souhaites-tu que je le crée et que j’y range ce document ?",
                expected.label
            )
        }
    } else if formal {
        format!(
            "En revanche, il se trouvait dans « {actual_label} », et non dans « {} ». Souhaitez-vous que je l’y range ?",
            expected.label
        )
    } else {
        format!(
            "En revanche, il se trouvait dans « {actual_label} », et non dans « {} ». Souhaites-tu que je l’y range ?",
            expected.label
        )
    };
    Ok(Some((
        initiative,
        PendingRef {
            action_id,
            tool: tool.into(),
            preview,
            risk_class: risk.as_str().into(),
        },
    )))
}

fn is_code_file_request(query: &str) -> bool {
    let query = crate::db::fold(query);
    [
        "code",
        "source",
        "script",
        "readme",
        "projet de developpement",
        "fichier ts",
        "fichier rust",
        "fichier python",
    ]
    .iter()
    .any(|term| query.contains(term))
}

fn file_matches_requested_domain(query: &str, result: &retrieval::Retrieved) -> bool {
    if is_code_file_request(query) {
        return true;
    }
    let path = result.path.as_deref().unwrap_or(&result.source_ref);
    let path = std::path::Path::new(path);
    !crate::connectors::files::is_project_root(path)
        && !crate::connectors::files::is_project_content(path)
}

fn filter_file_domain(query: &str, results: &mut Vec<retrieval::Retrieved>) {
    results.retain(|result| file_matches_requested_domain(query, result));
}

fn is_explicit_chat_confirmation(text: &str) -> bool {
    let text = crate::db::fold(text);
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if [
        "pas", "annule", "refuse", "change", "modifie", "corrige", "mais",
    ]
    .iter()
    .any(|term| words.contains(term))
    {
        return false;
    }
    [
        "je confirme",
        "je valide",
        "j'autorise",
        "je viens de t'autoriser",
        "tu peux envoyer",
        "envoie maintenant",
        "vas-y",
        "vas y",
    ]
    .iter()
    .any(|term| text.contains(term))
        || matches!(text.trim(), "oui" | "confirme" | "valide")
}

fn is_mail_content_followup(text: &str) -> bool {
    let text = crate::db::fold(text);
    [
        "dis-lui",
        "dis lui",
        "dis-leur",
        "dis leur",
        "le message",
        "le contenu",
        "ecris-lui",
        "ecris lui",
    ]
    .iter()
    .any(|term| text.contains(term))
}

async fn confirm_pending_mail_from_chat(
    core: &Core,
    session_id: &str,
    user_text: &str,
    settings: &crate::settings::Settings,
) -> Result<Option<Answer>> {
    if !is_explicit_chat_confirmation(user_text) {
        return Ok(None);
    }
    let matching: Vec<_> = actions::list_pending(&core.db)?
        .into_iter()
        .filter(|action| {
            action.tool == "mail.send" && action.session_id.as_deref() == Some(session_id)
        })
        .collect();
    if matching.len() != 1 {
        return Ok(None);
    }
    let pending = &matching[0];
    let action = actions::get_action(&core.db, &pending.id)?;
    let ctx = crate::tools::ToolCtx {
        db: core.db.clone(),
        llm: core.llm.clone(),
        bus: core.bus.clone(),
        settings: settings.clone(),
    };
    emit_progress(
        core,
        session_id,
        "execute",
        "Confirmation reçue, envoi du mail",
        Some(pending.preview.clone()),
        4,
        5,
        "running",
    );
    match crate::tools::execute(&ctx, &action.tool, &action.input).await {
        Ok(outcome) => {
            actions::set_action_result(
                &core.db,
                &pending.id,
                "executed",
                Some(
                    &outcome
                        .result
                        .to_string()
                        .chars()
                        .take(800)
                        .collect::<String>(),
                ),
                outcome.undo.as_ref(),
            )?;
            core.bus.emit(BusEvent::ActionResolved {
                action_id: pending.id.clone(),
                status: "executed".into(),
            });
            let to = outcome.result["to"].as_str().unwrap_or("son destinataire");
            let text = format!("C’est envoyé à {to}.");
            memory::persist_turn(&core.db, session_id, "assistant", &text)?;
            emit_progress(
                core,
                session_id,
                "complete",
                "Mail envoyé",
                Some(to.into()),
                5,
                5,
                "done",
            );
            Ok(Some(Answer {
                text,
                sources: vec![],
                pending_actions: vec![],
                session_id: session_id.into(),
                degraded: false,
            }))
        }
        Err(error) => {
            actions::set_action_result(
                &core.db,
                &pending.id,
                "failed",
                Some(&error.to_string()),
                None,
            )?;
            core.bus.emit(BusEvent::ActionResolved {
                action_id: pending.id.clone(),
                status: "failed".into(),
            });
            Err(error)
        }
    }
}

fn is_mail_composition_query(text: &str) -> bool {
    let text = crate::db::fold(text);
    let mentions_mail =
        text.contains("mail") || text.contains("email") || text.contains("courriel");
    let composes = ["envoie", "envoyer", "ecris", "redige", "reponds"]
        .iter()
        .any(|term| text.contains(term));
    mentions_mail && composes
}

/// Porte de complétude en amont de la porte de confirmation. Une confirmation
/// n'a de sens que pour un message réellement prêt et un destinataire établi.
fn mail_send_preflight(
    db: &crate::db::Db,
    args: &Value,
    current_user_text: &str,
    trusted_user_history: &str,
) -> Option<Value> {
    let to = args["to"].as_str().unwrap_or("").trim();
    let subject = args["subject"].as_str().unwrap_or("").trim();
    let body = args["body"].as_str().unwrap_or("").trim();

    if mail_request_missing_content(current_user_text) || subject.is_empty() || body.is_empty() {
        return Some(json!({
            "status": "incomplet",
            "missing": "message",
            "note": "N'appelle pas mail.send et ne crée aucune confirmation. Demande à l'utilisateur ce qu'il veut dire dans le mail."
        }));
    }
    if !(to.contains('@') && to.contains('.')) {
        return Some(json!({
            "status": "destinataire_non_resolu",
            "note": "Le destinataire n'est pas une adresse valide. Appelle people.resolve_email avec le nom donné, ou demande l'adresse."
        }));
    }
    let history_folded = crate::db::fold(trusted_user_history);
    if history_folded.contains(&crate::db::fold(to)) {
        return None;
    }
    match crate::connectors::people::email_is_known_for_mentioned_person(
        db,
        to,
        trusted_user_history,
    ) {
        Ok(true) => None,
        _ => Some(json!({
            "status": "destinataire_non_resolu",
            "rejected_address": to,
            "note": "Cette adresse n'a été ni donnée par l'utilisateur ni résolue depuis le contact nommé. Ne l'invente pas : appelle people.resolve_email ou demande l'adresse."
        })),
    }
}

fn mail_request_missing_content(text: &str) -> bool {
    if !is_mail_composition_query(text) {
        return false;
    }
    let text = crate::db::fold(text);
    let explicit_content = [
        " qui dit",
        " disant",
        " pour dire",
        " pour lui dire",
        " pour leur dire",
        "dis-lui",
        "dis lui",
        "dis-leur",
        "dis leur",
        "avec le message",
        "contenu",
        "objet",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    let quoted_or_separated = text.contains(':')
        || text.contains('«')
        || text.contains('"')
        || text
            .split_once(',')
            .is_some_and(|(_, tail)| tail.split_whitespace().count() >= 2);
    !explicit_content && !quoted_or_separated
}

fn cited_sources(
    text: &str,
    sources: &[retrieval::Retrieved],
) -> (String, Vec<retrieval::Retrieved>) {
    let marker = "[source:";
    let mut cursor = 0;
    let mut output = String::with_capacity(text.len());
    let mut selected: Vec<retrieval::Retrieved> = Vec::new();
    let mut numbering = std::collections::HashMap::<usize, usize>::new();

    while let Some(relative_start) = text[cursor..].find(marker) {
        let start = cursor + relative_start;
        output.push_str(&text[cursor..start]);
        let number_start = start + marker.len();
        let Some(relative_end) = text[number_start..].find(']') else {
            output.push_str(&text[start..]);
            cursor = text.len();
            break;
        };
        let end = number_start + relative_end;
        let original = &text[start..=end];
        let parsed = text[number_start..end].trim().parse::<usize>().ok();
        if let Some(index) = parsed.filter(|n| *n > 0 && *n <= sources.len()) {
            let new_number = if let Some(existing) = numbering.get(&index) {
                *existing
            } else {
                let next = selected.len() + 1;
                numbering.insert(index, next);
                selected.push(sources[index - 1].clone());
                next
            };
            output.push_str(&format!("[source:{new_number}]"));
        } else {
            output.push_str(original);
        }
        cursor = end + 1;
    }
    output.push_str(&text[cursor..]);
    (output, selected)
}

/// Condense les tours antérieurs à la fenêtre récente en un résumé stable :
/// faits, décisions, préférences, éléments en attente. Fusionne avec le
/// résumé précédent pour ne jamais perdre ce qui a déjà été condensé.
async fn summarize_session(
    db: &crate::db::Db,
    llm: &std::sync::Arc<dyn crate::llm::LlmClient>,
    session_id: &str,
) -> crate::error::Result<()> {
    let older = memory::older_turns(db, session_id, 12, 60)?;
    if older.len() < 4 {
        return Ok(());
    }
    let previous = memory::session_summary(db, session_id)?.unwrap_or_default();
    let mut transcript = String::new();
    for (role, content) in &older {
        let capped: String = content.chars().take(700).collect();
        transcript.push_str(&format!("{role} : {capped}\n"));
    }
    let system = "Tu condenses une conversation entre un utilisateur et son assistant. \
        Produis un résumé factuel en français (10 lignes max) : sujets traités, décisions prises, \
        préférences exprimées, informations personnelles durables, choses restées en attente. \
        Pas de préambule, pas de commentaire.";
    let user = if previous.is_empty() {
        format!("Conversation à condenser :\n{transcript}")
    } else {
        format!("Résumé existant (à fusionner, ne rien perdre d'important) :\n{previous}\n\nNouveaux échanges :\n{transcript}")
    };
    let resp = llm
        .generate(
            system,
            &[ChatMessage {
                role: "user".into(),
                content: user,
                tool_calls: None,
                tool_name: None,
            }],
            &[],
            GenParams {
                temperature: 0.2,
                max_tokens: Some(500),
                json: false,
            },
        )
        .await?;
    let summary = resp.content.trim();
    if !summary.is_empty() {
        memory::set_session_summary(db, session_id, summary)?;
    }
    Ok(())
}

fn explicit_action_intent(user_text: &str, tool: &str) -> bool {
    let text = user_text.to_lowercase();
    let verbs: &[&str] = match tool {
        // « mail » seul n'est PAS une intention d'envoi : « résume ce mail »
        // ne doit pas neutraliser le refus de provenance (audit §2).
        "mail.send" | "mail.draft" => &[
            "envoie",
            "envoyer",
            "envoi",
            "rédige",
            "redige",
            "brouillon",
            "réponds",
            "reponds",
        ],
        "calendar.create" | "calendar.update" | "calendar.delete" => &[
            "ajoute",
            "crée",
            "cree",
            "planifie",
            "programme",
            "déplace",
            "deplace",
            "annule",
        ],
        "tasks.create" | "tasks.complete" => {
            &["tâche", "tache", "rappelle", "ajoute", "termine", "marque"]
        }
        "memory.remember" => &["retiens", "mémorise", "memorise", "souviens"],
        "files.apply_reorganize_plan" => &[
            "range",
            "ranger",
            "organise",
            "réorganise",
            "reorganise",
            "confirme",
        ],
        "files.move" => &[
            "déplace",
            "deplace",
            "déplacer",
            "deplacer",
            "mets",
            "mettre",
            "range",
            "ranger",
        ],
        _ => &["fais", "effectue", "exécute", "execute"],
    };
    verbs.iter().any(|v| text.contains(v))
}

/// Réponse de repli quand le moteur d'inférence est indisponible :
/// on montre honnêtement les résultats du retrieval.
fn degraded_answer(ctx: &retrieval::ContextBundle, error: &str) -> String {
    let mut s = format!("⚠ {error}\n");
    if ctx.sources.is_empty() {
        s.push_str("La recherche locale n'a rien trouvé de pertinent non plus.");
    } else {
        s.push_str("Voici néanmoins ce que la recherche locale a trouvé :\n");
        for (i, src) in ctx.sources.iter().enumerate() {
            s.push_str(&format!(
                "{}. {} — {} [source:{}]\n",
                i + 1,
                src.title,
                src.source_ref,
                i + 1
            ));
        }
    }
    s
}

#[cfg(test)]
mod intent_tests {
    use super::{
        cited_sources, expected_folder_from_query, explicit_action_intent,
        file_matches_requested_domain, is_device_diagnostic_query, is_explicit_chat_confirmation,
        is_file_search_query, mail_request_missing_content, mail_send_preflight,
        resolve_file_search_request, screen_context_text,
    };
    use crate::db::Db;
    use crate::retrieval::Retrieved;

    fn source(title: &str) -> Retrieved {
        Retrieved {
            item_id: title.into(),
            source: "files".into(),
            source_ref: title.into(),
            title: title.into(),
            path: None,
            snippet: String::new(),
            score: 1.0,
        }
    }

    #[test]
    fn une_consigne_de_document_ne_vaut_pas_demande_utilisateur() {
        assert!(!explicit_action_intent(
            "Résume ce document",
            "tasks.create"
        ));
        assert!(explicit_action_intent(
            "Ajoute une tâche à partir de ce document",
            "tasks.create"
        ));
        assert!(explicit_action_intent(
            "Tu peux ranger le dossier USA dans Photos de famille ?",
            "files.move"
        ));
    }

    #[test]
    fn ne_surface_que_les_sources_reellement_citees() {
        let sources = vec![source("A"), source("B"), source("C")];
        let (text, selected) = cited_sources("Réponse système sans citation.", &sources);
        assert_eq!(text, "Réponse système sans citation.");
        assert!(selected.is_empty());

        let (text, selected) = cited_sources(
            "Selon C [source:3], puis A [source:1] et encore C [source:3].",
            &sources,
        );
        assert_eq!(
            text,
            "Selon C [source:1], puis A [source:2] et encore C [source:1]."
        );
        assert_eq!(
            selected
                .iter()
                .map(|s| s.title.as_str())
                .collect::<Vec<_>>(),
            vec!["C", "A"]
        );
    }

    #[test]
    fn les_metriques_machine_ninterrogent_pas_les_documents() {
        assert!(is_device_diagnostic_query(
            "Tu peux me donner les métriques de capacités de mon ordinateur ?"
        ));
        assert!(is_device_diagnostic_query(
            "Quelle est la température de mon Mac ?"
        ));
        assert!(!is_device_diagnostic_query(
            "Retrouve le document sur les capacités de mon ordinateur"
        ));
    }

    #[test]
    fn une_recherche_de_quittance_est_bien_une_recherche_de_fichier() {
        assert!(is_file_search_query(
            "Je cherche un document lié à ma quittance de loyer, tu peux me le retrouver ?"
        ));
        assert!(!is_file_search_query("Explique-moi le projet Aberration"));
    }

    #[test]
    fn comprend_un_emplacement_attendu_formule_naturellement() {
        let home = std::path::Path::new("/Users/alice");
        let expected = expected_folder_from_query(
            "Retrouve le rapport, je crois qu’il est dans un dossier Archives RH dans mes Documents",
            home,
        )
        .unwrap();
        assert_eq!(
            expected.path,
            std::path::Path::new("/Users/alice/Documents/Archives RH")
        );
        assert_eq!(expected.label, "~/Documents/Archives RH");

        assert!(expected_folder_from_query(
            "Tu peux retrouver le document sur la PSSI de mon entreprise ?",
            home
        )
        .is_none());
    }

    #[test]
    fn une_correction_conserve_la_recherche_documentaire_precedente() {
        let conversation: Vec<(String, String)> = vec![
            (
                "user".into(),
                "Retrouve un document lié à ma quittance de loyer".into(),
            ),
            (
                "assistant".into(),
                "Voici README.md et le projet Aberration".into(),
            ),
            (
                "user".into(),
                "Ces fichiers n'ont rien à voir avec une quittance !".into(),
            ),
        ];
        let request = resolve_file_search_request(&conversation[2].1, &conversation).unwrap();
        assert_eq!(request.0, conversation[0].1);
        assert!(request.1);

        let exact_repro: Vec<(String, String)> = vec![
            (
                "user".into(),
                "Tu peux me retrouver un document en lien avec ma quittance de loyer ?".into(),
            ),
            ("assistant".into(), "Voici plusieurs captures".into()),
            (
                "user".into(),
                "Le document que je cherche n'est pas dans cette liste, certains fichiers n'ont rien à voir".into(),
            ),
        ];
        let request = resolve_file_search_request(&exact_repro[2].1, &exact_repro).unwrap();
        assert_eq!(request.0, exact_repro[0].1);
        assert!(request.1);
    }

    #[test]
    fn un_readme_de_projet_est_exclu_dune_recherche_de_quittance() {
        let root = std::env::temp_dir().join(format!("syn-domain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        let readme = root.join("README.md");
        let candidate = Retrieved {
            item_id: "readme".into(),
            source: "files".into(),
            source_ref: readme.to_string_lossy().into(),
            title: "README.md".into(),
            path: Some(readme.to_string_lossy().into()),
            snippet: "Test de recherche d'une quittance de loyer".into(),
            score: 1.0,
        };
        assert!(!file_matches_requested_domain(
            "Trouve ma quittance de loyer",
            &candidate
        ));
        assert!(file_matches_requested_domain(
            "Trouve le README de mon projet de code",
            &candidate
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn un_mail_sans_message_doit_rester_incomplet() {
        assert!(mail_request_missing_content("Envoie un mail à Paul"));
        assert!(!mail_request_missing_content(
            "Envoie un mail à Paul qui dit que j'arrive à 18 heures"
        ));
        assert!(!mail_request_missing_content("Dis-lui simplement bonjour"));
    }

    #[test]
    fn une_confirmation_naturelle_est_reconnue_sans_confondre_une_correction() {
        assert!(is_explicit_chat_confirmation(
            "Je viens de t'autoriser, tu peux envoyer maintenant ?"
        ));
        assert!(is_explicit_chat_confirmation("Oui"));
        assert!(!is_explicit_chat_confirmation(
            "Je confirme mais change d'abord l'objet"
        ));
        assert!(!is_explicit_chat_confirmation("N'envoie pas"));
    }

    #[test]
    fn une_adresse_inventee_est_bloquee_et_un_contact_connu_est_accepte() {
        let dir = std::env::temp_dir().join(format!("syn-mail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        let args = serde_json::json!({
            "to": "paul@example.com",
            "subject": "Bonjour",
            "body": "Je passerai à 18 heures."
        });
        let user = "Envoie un mail à Paul qui dit que je passerai à 18 heures";
        let rejected = mail_send_preflight(&db, &args, user, user).unwrap();
        assert_eq!(rejected["status"], "destinataire_non_resolu");

        crate::memory::find_or_create_person(&db, "Paul", Some("paul@exemple.fr"), None).unwrap();
        let known = serde_json::json!({
            "to": "paul@exemple.fr",
            "subject": "Bonjour",
            "body": "Je passerai à 18 heures."
        });
        assert!(mail_send_preflight(&db, &known, user, user).is_none());
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_contexte_ecran_ne_confond_pas_syn_et_lapplication_cible() {
        let context = serde_json::json!({
            "available": true,
            "app": "Code",
            "window": "main.rs — Visual Studio Code",
            "text": "[haut gauche] Syn\n[milieu gauche] gh auth login"
        });
        let text = screen_context_text(&context).unwrap();
        assert!(text.contains("fenêtres de Syn : Code"));
        assert!(text.contains("présence du mot « Syn »"));
        assert!(text.contains("Visual Studio Code"));
    }
}
