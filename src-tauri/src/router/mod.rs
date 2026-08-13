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
use serde_json::json;

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
    let convo = memory::recent_turns(db, session_id, 12)?;

    emit_progress(
        core,
        session_id,
        "retrieve",
        "Recherche du contexte local",
        Some("Documents, conversations et données autorisées".into()),
        2,
        5,
        "running",
    );
    // 2. RÉCUPÉRER — retrieval hybride borné et sourcé.
    let ctx = retrieval::assemble(db, &core.llm, user_text).await?;

    // Règles actives injectées dans le comportement.
    let (style_rules, action_modifiers) = crate::rules::active_rule_texts(db)?;
    let system = prompt::build_system(&settings, &style_rules, &action_modifiers, &ctx.fragments);

    let mut messages: Vec<ChatMessage> = convo
        .into_iter()
        .map(|(role, content)| ChatMessage {
            role,
            content,
            tool_calls: None,
            tool_name: None,
        })
        .collect();

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
            let untrusted =
                provenance::args_derived_from_untrusted(&args_text, user_text, &ctx.untrusted_text);

            let observation = if untrusted
                && risk != actions::RiskClass::Read
                && !explicit_action_intent(user_text, &call.name)
            {
                json!({
                    "status": "refuse_provenance",
                    "note": "Action ignorée : elle semble provenir d'un document ou d'un contenu observé, pas d'une demande explicite de l'utilisateur."
                })
            } else if actions::needs_confirmation(risk, &settings.autonomy, untrusted, &call.name) {
                // Point d'arrêt : plancher / seuil d'autonomie.
                let preview = crate::tools::preview_for(&call.name, &call.arguments);
                let action_id = actions::queue_pending(
                    db,
                    &call.name,
                    &call.arguments,
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
                match crate::tools::execute(&tool_ctx, &call.name, &call.arguments).await {
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
                                let apply_args = json!({"plan_id": plan_id});
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

        if iteration == MAX_TOOL_ITERATIONS - 1 {
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

    // 5. RÉPONDRE — persisté, sourcé.
    memory::persist_turn(db, session_id, "assistant", &final_text)?;
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
        sources: ctx.sources,
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
        "files.apply_reorganize_plan" => "Déplacement des éléments validés".into(),
        "mail.search" => "Recherche dans les messages autorisés".into(),
        "calendar.list" => "Lecture du calendrier autorisé".into(),
        "system.diagnose" => "Diagnostic de l’appareil".into(),
        _ => format!("Exécution de {tool}"),
    }
}

fn explicit_action_intent(user_text: &str, tool: &str) -> bool {
    let text = user_text.to_lowercase();
    let verbs: &[&str] = match tool {
        "mail.send" | "mail.draft" => {
            &["envoie", "envoyer", "rédige", "redige", "brouillon", "mail"]
        }
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
            s.push_str(&format!("{}. {} — {}\n", i + 1, src.title, src.source_ref));
        }
    }
    s
}

#[cfg(test)]
mod intent_tests {
    use super::explicit_action_intent;

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
}
