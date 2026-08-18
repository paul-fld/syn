//! La boucle agentique (Intelligence §5) : perceive → retrieve → plan → act →
//! observe → respond, orchestrée en Rust. Le modèle n'orchestre pas — il est
//! appelé À L'INTÉRIEUR de la boucle. La confirmation est un point d'arrêt
//! DANS la boucle (plancher humain), pas une couche UI par-dessus.
//!
//! # Le modèle comprend, le déterministe garantit
//!
//! Cette ligne de partage n'est pas un principe abstrait : chaque fois qu'elle
//! a été franchie, un défaut est apparu. Elle s'énonce en deux règles.
//!
//! **Comprendre est le travail du modèle.** De quoi l'utilisateur parle, ce
//! qu'il veut, si sa réponse est un accord ou une correction, quel compte il
//! désigne : tout cela relève du sens, et le sens ne se lit pas dans une liste
//! de mots. « Demande-lui s'il est d'accord » n'est pas un accord ; « tu peux
//! envoyer un courriel à Julie » n'est pas une confirmation d'envoi. Les
//! fonctions à mots-clés de ce fichier ne servent plus qu'à DEUX choses : tenir
//! le service quand le modèle est arrêté (`intent::Source::Fallback`), et
//! reconnaître des noms propres d'un inventaire fermé (Gmail, OneDrive…).
//! Aucune ne doit décider à la place d'une compréhension disponible.
//!
//! **Garantir est le travail du code.** La légitimité d'un destinataire, le
//! plancher de confirmation, la vérification qu'un envoi affirmé a bien eu
//! lieu, la provenance d'une action dérivée d'un contenu observé : ce sont des
//! garanties, pas des interprétations. Elles ne passent jamais par le modèle,
//! et une compréhension erronée ne doit jamais pouvoir les contourner.
//!
//! Corollaire de conception : rien d'irréversible ne dépend d'une
//! compréhension. Au pire, une erreur de sens produit un échange maladroit —
//! jamais une action non voulue. C'est ce qui rend acceptable de confier le
//! sens à un modèle local faillible.
//!
//! Les deux mesures qui tiennent cette ligne vivante :
//! `mesure_du_taux_derreur_avec_comprehension` (aiguillage) et
//! `mesure_des_reponses_en_cours_de_parcours` (réponses en cours de parcours).
//! Toutes deux exigent `--test-threads=1`.

#[cfg(test)]
pub mod eval;
pub mod intent;
pub mod mail_flow;
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
    /// Choix proposés avec la réponse (comptes d'envoi d'un mail…). L'interface
    /// les affiche en boutons : une question fermée ne se tape pas au clavier.
    pub choices: Vec<mail_flow::AccountChoice>,
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
    // L'étape que Syn attend, s'il en attend une. Elle part AVEC la question au
    // modèle : « oui » n'a de sens que rapporté à ce qui vient d'être demandé.
    let etape = mail_flow::pending_step(db, session_id)?;
    let convo = memory::recent_turns(db, session_id, 12)?;
    // Copie de la parole utilisateur avant ajout éventuel du contexte d'écran :
    // elle sert à vérifier qu'un destinataire vient bien d'un canal fiable.
    let trusted_user_history = convo
        .iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    // Compréhension de l'intention. Les portes à mots-clés qui suivent ne
    // servent plus qu'à tenir le service quand le modèle local est arrêté :
    // exiger « cherche » ou « document » pour comprendre une demande revenait à
    // ne servir que les utilisateurs qui parlent comme le code est écrit.
    let keyword_request = resolve_file_search_request(user_text, &convo);
    let understood = intent::classify(
        &core.llm,
        user_text,
        // Le tour courant est déjà persisté : on ne le remet pas en contexte
        // de lui-même.
        convo.split_last().map(|(_, avant)| avant).unwrap_or(&[]),
        etape,
        intent::fallback(
            user_text,
            keyword_request.clone(),
            match file_search_scope(user_text) {
                FileSearchScope::Cloud(Some("google")) => intent::Scope::Google,
                FileSearchScope::Cloud(Some("microsoft")) => intent::Scope::Microsoft,
                FileSearchScope::Cloud(_) => intent::Scope::AnyCloud,
                FileSearchScope::Local => intent::Scope::Local,
                FileSearchScope::Federated(_) => intent::Scope::Any,
            },
            is_device_diagnostic_query(user_text),
            is_mail_composition_query(user_text),
            is_mail_search_query(user_text),
        ),
    )
    .await;

    // Une étape en cours se résout avec ce que le modèle a compris de la
    // réponse — accord, correction, compte désigné. Les listes de mots ne
    // servent plus que si le modèle n'a rien pu dire (arrêté, ou dépassé).
    let lecture = etape.map(|etape| {
        mail_flow::read_reply(
            etape,
            understood.reply,
            user_text,
            &crate::connectors::mail::available_channels(db),
        )
    });
    if let (Some(etape), Some(lecture)) = (etape, lecture) {
        // Confirmer un envoi par le fil de conversation : c'est le geste le plus
        // conséquent du parcours, et il dépendait d'une liste de mots capable de
        // faire partir un mail sur « tu peux envoyer un courriel à Julie… ».
        if etape == intent::Step::SendConfirmation {
            if lecture == intent::Reply::Accord {
                if let Some(answer) =
                    confirm_pending_mail_from_chat(core, session_id, &settings).await?
                {
                    return Ok(answer);
                }
            }
        } else if let Some(answer) =
            mail_flow::handle_user_reply(core, session_id, etape, lecture, &settings)?
        {
            return Ok(answer);
        }
    }

    // Une correction (« ces fichiers n'ont rien à voir ») porte sur la recherche
    // précédente, pas sur une intention nouvelle : elle reprend le sujet du
    // tour d'avant. Mais elle ne PRIME plus sur la compréhension — un « ça n'a
    // rien à voir » lâché en pleine rédaction de mail rejouait une recherche de
    // documents, quoi que le modèle ait compris.
    let correction = keyword_request
        .as_ref()
        .filter(|(_, is_correction)| *is_correction)
        .cloned();
    let file_request = match understood.kind {
        intent::Kind::FileSearch => Some(match &correction {
            Some(correction) => correction.clone(),
            None => (
                understood
                    .subject
                    .clone()
                    .unwrap_or_else(|| requested_document_query(user_text)),
                false,
            ),
        }),
        // Hors ligne, la compréhension retombe sur les mots-clés : une
        // correction reconnue reste alors le seul indice disponible.
        _ if understood.source == intent::Source::Fallback => correction.clone(),
        _ => None,
    };
    let device_only = understood.kind == intent::Kind::DeviceDiagnostic;
    let file_search = file_request.is_some();
    // L'intention comprise fait foi. Le repli à mots-clés ne sert plus qu'à
    // rattraper le cas où le modèle n'a rien pu dire.
    let mail_composition = understood.kind == intent::Kind::MailCompose
        || (understood.source == intent::Source::Fallback
            && is_mail_content_followup(user_text)
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
    // Retrouver un message reçu est un parcours à part : il se cherche dans les
    // messageries, pas dans l'index de fichiers.
    if understood.kind == intent::Kind::MailSearch {
        let target = understood
            .subject
            .clone()
            .filter(|subject| subject.trim().len() >= 3)
            .unwrap_or_else(|| user_text.to_string());
        let action = understood
            .mail_action
            .unwrap_or_else(|| mail_action_fallback(user_text));
        return match action {
            intent::MailAction::Lister => {
                answer_mail_list(core, session_id, user_text, settings.voice.vouvoie()).await
            }
            _ => {
                answer_mail_search(
                    core,
                    session_id,
                    &target,
                    understood.scope,
                    action,
                    settings.voice.vouvoie(),
                )
                .await
            }
        };
    }
    if let Some((query, is_correction)) = file_request {
        return answer_file_search(
            core,
            session_id,
            &query,
            understood.scope,
            is_correction,
            settings.voice.formality == "vous",
        )
        .await;
    }
    let composition_en_cours = crate::connectors::mail::composition(db, session_id)?;
    // Une seule question à la fois, et dans l'ordre : à QUI d'abord, QUOI
    // ensuite. Tant que le destinataire n'est pas établi, le tour revient au
    // modèle pour qu'il résolve le nom en adresse et la fasse confirmer —
    // demander le contenu d'un mail dont on ignore le destinataire mettait
    // l'utilisateur devant une question prématurée.
    let destinataire_connu = !composition_en_cours.recipient.is_empty() || user_text.contains('@');
    if mail_composition
        && destinataire_connu
        && mail_request_missing_content(user_text)
        && composition_en_cours.body.is_empty()
    {
        let text = settings
            .voice
            .pick(
                "Que veux-tu dire dans ce mail ? Je n’ai encore préparé ni envoyé aucun message.",
                "Que voulez-vous dire dans ce mail ? Je n’ai encore préparé ni envoyé aucun message.",
            )
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
            choices: vec![],
            session_id: session_id.into(),
            degraded: false,
        });
    }
    // Le destinataire est connu et l'utilisateur a dit ce qu'il veut dire :
    // Syn rédige LUI-MÊME et enchaîne sur la relecture. Confier cette étape au
    // modèle par un appel d'outil ne marchait pas — il écrivait le mail dans sa
    // réponse, puis prétendait l'avoir envoyé sans qu'aucun outil ait tourné.
    if mail_composition && destinataire_connu {
        // Réécrire, mais seulement si l'utilisateur l'a demandé. Se fier à
        // « un texte attend une relecture » suffisait à faire réécrire le mail
        // sur une phrase qui parlait d'autre chose.
        let correction_demandee = lecture == Some(intent::Reply::Correction);
        let premiere_redaction =
            composition_en_cours.body.is_empty() && mail_content_expressed(user_text, &understood);
        if correction_demandee || premiere_redaction {
            if let Some(answer) = mail_flow::compose(
                core,
                session_id,
                user_text,
                &trusted_user_history,
                &settings,
            )
            .await?
            {
                return Ok(answer);
            }
        }
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

    // Les documents confiés à cette conversation entrent dans le contexte de
    // CHAQUE tour : l'utilisateur les a sous les yeux, il parle de ceux-là.
    // Les faire dépendre de la recherche reviendrait à remettre au hasard ce
    // qu'il vient de donner.
    let joints = crate::tools::attachments::context_fragments(db, session_id)?;
    for fragment in &joints {
        let citation = ctx.fragments.len() + 1;
        ctx.fragments.push((citation, fragment.clone()));
        ctx.untrusted_text.push_str(fragment);
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

    // Le catalogue est restreint à l'intention comprise : c'est le principal
    // levier de réactivité de la boucle agentique.
    let catalog = crate::tools::catalog_for(understood.kind);
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
        // Diffusion au fil de l'eau : l'utilisateur voit la réponse s'écrire au
        // lieu d'attendre un bloc. Le relais est refermé à la fin de l'étape,
        // ce qui termine la tâche d'émission sans la laisser filer.
        let (sink, mut deltas) = tokio::sync::mpsc::unbounded_channel::<String>();
        let delta_bus = core.bus.clone();
        let delta_session = session_id.to_string();
        let relay = tauri::async_runtime::spawn(async move {
            while let Some(delta) = deltas.recv().await {
                delta_bus.emit(BusEvent::AnswerDelta {
                    session_id: delta_session.clone(),
                    delta,
                });
            }
        });
        let generated = core
            .llm
            .generate_streaming(
                &system,
                &messages,
                &catalog,
                GenParams {
                    temperature: 0.3,
                    // Le modèle écrit à une vingtaine de jetons par seconde :
                    // 1200 jetons autorisaient une minute d'écriture pour UNE
                    // réponse. Un assistant qui répond en trois phrases n'en a
                    // jamais besoin, et le plafond protège contre les tirades.
                    max_tokens: Some(500),
                    json: false,
                },
                sink,
            )
            .await;
        relay.await.ok();
        let resp = match generated {
            Ok(r) => r,
            Err(e) => {
                // Mode dégradé (doc maître §22) : le retrieval fonctionne,
                // la génération est signalée indisponible.
                degraded = true;
                final_text = degraded_answer(&ctx, &e.to_string());
                break;
            }
        };

        // Un appel d'outil écrit en texte reste un appel d'outil : l'exécuter
        // plutôt que d'afficher son JSON à l'utilisateur.
        let mut resp = resp;
        if resp.tool_calls.is_empty() {
            if let Some(recovered) = crate::llm::tool_call_from_text(&resp.content) {
                resp.tool_calls.push(recovered);
                resp.content.clear();
            }
        }
        if resp.tool_calls.is_empty() {
            // Reste-t-il du JSON non récupérable ? Ne jamais le montrer : on
            // relance une fois en demandant une phrase, puis on renonce
            // proprement.
            if crate::llm::looks_structured(&resp.content) {
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: "Réponds à l'utilisateur en une phrase, sans JSON ni appel d'outil."
                        .into(),
                    tool_calls: None,
                    tool_name: None,
                });
                continue;
            }
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

            // Un envoi se construit sur plusieurs tours. On accumule les
            // arguments STRUCTURÉS déjà fournis et on complète l'appel courant :
            // le modèle n'a plus besoin de se souvenir de tout, et l'utilisateur
            // n'a plus à répéter ce qu'il a déjà dit.
            let mut verified_arguments = call.arguments.clone();
            // Certains outils rangent leur résultat DANS la conversation (une
            // pièce jointe importée en devient un document) : ils ont besoin de
            // savoir laquelle.
            if call.name == "mail.attachments" {
                verified_arguments["_syn_session"] = json!(session_id);
            }
            if matches!(call.name.as_str(), "mail.send" | "mail.draft") {
                let known =
                    crate::connectors::mail::remember_composition(db, session_id, &call.arguments)?;
                for (key, value) in [
                    ("to", &known.recipient),
                    ("subject", &known.subject),
                    ("body", &known.body),
                    ("via", &known.via),
                ] {
                    if verified_arguments[key]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                        && !value.is_empty()
                    {
                        verified_arguments[key] = json!(value);
                    }
                }
            }
            let composition_state =
                crate::connectors::mail::composition(db, session_id).unwrap_or_default();
            let mail_preflight = if call.name == "mail.send" {
                let to = verified_arguments["to"].as_str().unwrap_or("").trim();
                mail_send_preflight(
                    db,
                    &verified_arguments,
                    user_text,
                    &trusted_user_history,
                    !composition_state.body.is_empty(),
                    // L'adresse sortie du carnet d'adresses de l'utilisateur est
                    // légitime par construction : c'est le parcours normal, et
                    // le contrôle la refusait.
                    composition_state.recipient_is_resolved()
                        && composition_state.recipient.eq_ignore_ascii_case(to),
                )
            } else {
                None
            };
            // Le modèle rédige parfois le mail avec mail.draft au lieu de
            // mail.send, puis demande en prose « je l'envoie ou tu le modifies ? ».
            // L'utilisateur n'a alors jamais vu son texte. Un brouillon ne se
            // crée que s'il l'a demandé : sinon c'est une rédaction, et la suite
            // appartient au parcours.
            if call.name == "mail.draft"
                && understood.kind == intent::Kind::MailCompose
                && !asked_for_draft(user_text)
                && !asked_for_draft(&trusted_user_history)
            {
                if let Some(answer) =
                    mail_flow::advance(core, session_id, &settings, &trusted_user_history)?
                {
                    return Ok(answer);
                }
            }
            // Le modèle a fait sa part (destinataire, objet, corps). La suite —
            // relecture, compte d'envoi, carte de confirmation — est un parcours
            // fixe : Syn le mène lui-même, dans l'ordre des maquettes.
            if call.name == "mail.send"
                && matches!(
                    mail_preflight
                        .as_ref()
                        .map(|reason| reason["status"].as_str().unwrap_or_default()),
                    None | Some("compte_a_choisir")
                )
            {
                if let Some(answer) =
                    mail_flow::advance(core, session_id, &settings, &trusted_user_history)?
                {
                    return Ok(answer);
                }
            }
            if call.name == "mail.send" && mail_preflight.is_none() {
                // Marqueur interne ajouté uniquement après les contrôles ci-dessus.
                // Les anciennes actions en attente (créées avant ce correctif)
                // ne peuvent ainsi pas envoyer une adresse inventée au clic.
                verified_arguments["_syn_preflight_v1"] = json!(true);
                // Un seul compte disponible : inutile de demander, mais le
                // choix devient explicite dans l'aperçu de confirmation.
                if verified_arguments["via"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    if let Some((id, _)) = crate::connectors::mail::available_channels(db)
                        .first()
                        .copied()
                    {
                        verified_arguments["via"] = json!(id);
                    }
                }
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
                // L'identifiant reste interne : donné au modèle, il finissait
                // recopié dans la réponse (« appelez files.cancel avec
                // l'action_id … »). L'interface gère la confirmation, le modèle
                // n'a qu'à annoncer l'attente.
                json!({"status": "en_attente_de_confirmation",
                       "note": "L'utilisateur doit confirmer cette action dans l'interface. Dis-le-lui en une phrase, sans identifiant ni nom d'outil."})
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
                        if call.name == "mail.send" {
                            crate::connectors::mail::clear_composition(db, session_id)?;
                        }
                        // L'adresse trouvée dans le carnet entre dans l'état
                        // d'envoi par une porte de confiance, distincte des
                        // arguments écrits par le modèle.
                        if call.name == "people.resolve_email"
                            && understood.kind == intent::Kind::MailCompose
                            && result["resolved"].as_bool() == Some(true)
                        {
                            if let Some(email) = result["matches"][0]["email"].as_str() {
                                crate::connectors::mail::remember_resolved_recipient(
                                    db, session_id, email,
                                )?;
                            }
                        }
                        // Un envoi vers une adresse inconnue est une occasion
                        // d'apprendre — mais jamais en silence. Syn propose,
                        // l'utilisateur tranche : une mémoire qui se remplit
                        // toute seule finit par contenir surtout du bruit.
                        if matches!(call.name.as_str(), "mail.send" | "mail.draft") {
                            if let Some((name, email)) = learnable_contact(db, &verified_arguments)?
                            {
                                let link_args = json!({"name": name, "email": email});
                                let preview =
                                    crate::tools::preview_for("people.link_email", &link_args);
                                let action_id = actions::queue_pending(
                                    db,
                                    "people.link_email",
                                    &link_args,
                                    actions::classify("people.link_email", &link_args),
                                    &preview,
                                    false,
                                    Some(session_id),
                                )?;
                                core.bus.emit(BusEvent::ActionAwaitingConfirmation {
                                    action_id: action_id.clone(),
                                    tool: "people.link_email".into(),
                                    preview: preview.clone(),
                                    risk_class: actions::RiskClass::ReversibleLocal.as_str().into(),
                                });
                                pending.push(PendingRef {
                                    action_id,
                                    tool: "people.link_email".into(),
                                    preview,
                                    risk_class: actions::RiskClass::ReversibleLocal.as_str().into(),
                                });
                                result["memoire_proposee"] = json!({
                                    "note": "Une association nom/adresse est proposée à l'utilisateur. Mentionne-la en une phrase, sans la présenter comme acquise."
                                });
                            }
                        }
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

    // Le modèle annonce parfois l'action au lieu de la faire : « Je vais
    // envoyer ce mail… », puis plus rien, et l'utilisateur reste devant une
    // promesse. Quand tout est connu, la suite du parcours ne dépend plus de sa
    // bonne volonté — Syn la mène lui-même. Rien n'est envoyé pour autant :
    // l'utilisateur garde le dernier mot.
    let envoi_deja_en_attente = actions::list_pending(db)?.iter().any(|action| {
        action.tool == "mail.send" && action.session_id.as_deref() == Some(session_id)
    });
    if understood.kind == intent::Kind::MailCompose
        && !pending.iter().any(|action| action.tool == "mail.send")
        // Une carte attend déjà : l'utilisateur a la main, on ne recouvre pas
        // la réponse du modèle par un rappel qu'il a sous les yeux.
        && !envoi_deja_en_attente
    {
        if let Some(answer) =
            mail_flow::advance(core, session_id, &settings, &trusted_user_history)?
        {
            return Ok(answer);
        }
    }

    // Un envoi affirmé sans envoi réel est le pire des défauts : l'utilisateur
    // croit son message parti. Le modèle l'a fait — « Le mail a été envoyé à
    // … » alors qu'aucun outil n'avait tourné. Une consigne ne suffit pas :
    // on confronte l'affirmation au journal des actions.
    if claims_a_sent_mail(&final_text) && !mail_really_sent(db, session_id)? {
        // On ne se contente pas de démentir : on reprend le parcours là où il en
        // est réellement — relecture, choix du compte, ou carte de confirmation.
        let reprise = match mail_flow::advance(core, session_id, &settings, &trusted_user_history)?
        {
            Some(answer) => Some(answer),
            None => {
                mail_flow::compose(
                    core,
                    session_id,
                    user_text,
                    &trusted_user_history,
                    &settings,
                )
                .await?
            }
        };
        if let Some(answer) = reprise {
            return Ok(answer);
        }
        final_text = settings
            .voice
            .pick(
                "Je n'ai encore rien envoyé. Dis-moi ce que le message doit dire et je le prépare.",
                "Je n'ai encore rien envoyé. Dites-moi ce que le message doit dire et je le prépare.",
            )
            .to_string();
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
    final_text = strip_internal_noise(&final_text);
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
        choices: vec![],
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
        "ressors",
        "ressortir",
        "ouvre",
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
        "word",
        "excel",
        "powerpoint",
        "power point",
        "google docs",
        "google slides",
        "google sheets",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSearchScope {
    Local,
    Cloud(Option<&'static str>),
    /// Aucun emplacement imposé : toutes les sources sont interrogées. Le
    /// fournisseur optionnel n'est qu'un signal de classement (ex. Word).
    Federated(Option<&'static str>),
}

fn file_search_scope(query: &str) -> FileSearchScope {
    let folded = crate::db::fold(query);
    if folded.contains("google docs")
        || folded.contains("google doc")
        || folded.contains("google slides")
        || folded.contains("google sheets")
        || folded.contains("google drive")
        || folded.contains("dans drive")
    {
        FileSearchScope::Cloud(Some("google"))
    } else if folded.contains("onedrive")
        || folded.contains("microsoft 365")
        || folded.contains("sharepoint")
    {
        FileSearchScope::Cloud(Some("microsoft"))
    } else if folded.contains("dans le cloud") || folded.contains("sur le cloud") {
        FileSearchScope::Cloud(None)
    } else if folded.contains("sur mon mac")
        || folded.contains("sur mon disque")
        || folded.contains("sur le disque")
        || folded.contains("en local")
        || folded.contains("fichier local")
    {
        FileSearchScope::Local
    } else if ["word", "excel", "powerpoint", "power point"]
        .iter()
        .any(|product| folded.contains(product))
    {
        FileSearchScope::Federated(Some("microsoft"))
    } else {
        FileSearchScope::Federated(None)
    }
}

/// Extrait le sujet du document sans transmettre à Drive toute la phrase
/// conversationnelle ("peux-tu me ressortir… qui se trouve dans…").
fn requested_document_query(query: &str) -> String {
    // Segmentation par classes de mots : aucune tournure n'est énumérée ici, la
    // même règle vaut pour « ressors-moi le document du X qui se trouve dans mes
    // Google Docs » que pour « where is the Q3 forecast ».
    if let Some(subject) = retrieval::subject_span(query) {
        if subject.chars().count() >= 2 {
            return subject;
        }
    }
    // Aucun mot porteur (« ouvre-le », « et celui-là ? ») : on rend la demande
    // telle quelle plutôt que d'inventer un sujet.
    query.trim().to_string()
}

/// `query` est ici le SUJET déjà extrait de la demande, et `scope` la portée
/// que l'utilisateur a lui-même nommée. Ni l'un ni l'autre n'est re-dérivé de
/// la phrase : la compréhension a eu lieu une fois, en amont.
async fn answer_file_search(
    core: &Core,
    session_id: &str,
    query: &str,
    scope: intent::Scope,
    is_correction: bool,
    formal: bool,
) -> Result<Answer> {
    let resolved = match scope {
        intent::Scope::Google => FileSearchScope::Cloud(Some("google")),
        intent::Scope::Microsoft => FileSearchScope::Cloud(Some("microsoft")),
        intent::Scope::AnyCloud => FileSearchScope::Cloud(None),
        intent::Scope::Local => FileSearchScope::Local,
        intent::Scope::Any => FileSearchScope::Federated(None),
    };
    match resolved {
        FileSearchScope::Cloud(provider) => {
            return answer_cloud_file_search(
                core,
                session_id,
                query,
                provider,
                is_correction,
                formal,
            )
            .await;
        }
        FileSearchScope::Federated(preferred) => {
            return answer_federated_file_search(
                core,
                session_id,
                query,
                preferred,
                is_correction,
                formal,
            )
            .await;
        }
        FileSearchScope::Local => {}
    }
    let mut results = retrieval::search_lexical_source(&core.db, query, 8, "files").await?;
    filter_file_domain(query, &mut results);

    // Filet de sécurité immédiat pendant la construction de l'index : cherche
    // les noms et dossiers directement sur le périmètre autorisé, puis programme
    // leur ingestion. Aucun chemin utilisateur ni cas métier n'est codé ici.
    let roots = crate::connectors::files::folder_paths(&core.db)?;
    let keywords = retrieval::keywords(query);
    let live_search = tokio::task::spawn_blocking(move || {
        crate::connectors::files::live_metadata_search(&roots, &keywords, 12)
    });
    let mut live_results =
        match tokio::time::timeout(std::time::Duration::from_millis(1_200), live_search).await {
            Ok(Ok(results)) => results,
            _ => Vec::new(),
        };
    filter_file_domain(query, &mut live_results);
    if !live_results.is_empty() {
        let live_paths = live_results
            .iter()
            .map(|result| std::path::PathBuf::from(&result.source_ref))
            .collect();
        let _ = core
            .indexer
            .tx
            .send(crate::connectors::files::IndexJob::Demand(live_paths));
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
            for result in retrieval::search_lexical_source(&core.db, &variant, 4, "files").await? {
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
    // La réponse lexicale part immédiatement. La voie sémantique est calculée
    // derrière un budget et streamée ensuite vers la conversation.
    let semantic_db = core.db.clone();
    let semantic_llm = core.llm.clone();
    let semantic_bus = core.bus.clone();
    let semantic_query = query.to_string();
    let semantic_session = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        for pass in 0..2 {
            if let Ok(Ok(results)) = tokio::time::timeout_at(
                deadline,
                retrieval::search_source(&semantic_db, &semantic_llm, &semantic_query, 8, "files"),
            )
            .await
            {
                semantic_bus.emit(BusEvent::SemanticResults {
                    session_id: semantic_session.clone(),
                    results,
                });
            }
            if pass == 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            }
        }
    });
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
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Fait entrer un résultat live dans le cache : il devient ouvrable (la garde de
/// périmètre d'`open_source` ne connaît que les objets indexés) et sa file
/// d'enrichissement téléchargera son contenu.
async fn remember_live_cloud(core: &Core, value: &serde_json::Value) {
    let settings = match crate::settings::load(&core.db) {
        Ok(settings) => settings,
        Err(_) => return,
    };
    let _ = crate::connectors::external::remember_live_result(
        &core.db,
        &core.llm,
        &core.bus,
        &settings.embed_model,
        value,
    )
    .await;
}

fn retrieved_from_live_cloud(value: &serde_json::Value) -> Option<retrieval::Retrieved> {
    let source_ref = value["source_ref"].as_str()?;
    Some(retrieval::Retrieved {
        item_id: value["item_id"].as_str().unwrap_or(source_ref).to_string(),
        source: "cloud".into(),
        source_ref: source_ref.to_string(),
        title: value["title"]
            .as_str()
            .unwrap_or("Document cloud")
            .to_string(),
        path: value["path"].as_str().map(str::to_string),
        snippet: value["snippet"].as_str().unwrap_or_default().to_string(),
        // Le connecteur classe déjà ses résultats par correspondance de titre ;
        // un score plat ferait remonter le premier venu au-dessus du bon.
        score: value["score"].as_f64().unwrap_or(5.0) as f32,
    })
}

async fn answer_federated_file_search(
    core: &Core,
    session_id: &str,
    query: &str,
    preferred_provider: Option<&'static str>,
    is_correction: bool,
    formal: bool,
) -> Result<Answer> {
    let target = query.to_string();
    let roots = crate::connectors::files::folder_paths(&core.db)?;
    let keywords = retrieval::keywords(&target);
    let disk_search = tokio::task::spawn_blocking(move || {
        crate::connectors::files::live_metadata_search(&roots, &keywords, 12)
    });
    let local_future = retrieval::search_lexical_source(&core.db, &target, 12, "files");
    let cloud_future = retrieval::search_lexical_source(&core.db, &target, 12, "cloud");
    let live_cloud_future = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        crate::connectors::external::live_search("cloud", &target),
    );
    let (local, cloud, disk, live_cloud) = tokio::join!(
        local_future,
        cloud_future,
        tokio::time::timeout(std::time::Duration::from_millis(1_200), disk_search),
        live_cloud_future,
    );

    let mut candidates = local?;
    filter_file_domain(&target, &mut candidates);
    let mut live_paths = Vec::new();
    if let Ok(Ok(mut disk_results)) = disk {
        filter_file_domain(&target, &mut disk_results);
        live_paths.extend(
            disk_results
                .iter()
                .map(|result| std::path::PathBuf::from(&result.source_ref)),
        );
        candidates.extend(disk_results);
    }
    if !live_paths.is_empty() {
        let _ = core
            .indexer
            .tx
            .send(crate::connectors::files::IndexJob::Demand(live_paths));
    }
    candidates.extend(cloud?);
    if let Ok(values) = live_cloud {
        for value in &values {
            remember_live_cloud(core, value).await;
        }
        candidates.extend(values.iter().filter_map(retrieved_from_live_cloud));
    }

    let mut by_ref = std::collections::HashMap::new();
    for mut candidate in candidates {
        if preferred_provider.is_some_and(|provider| candidate.source_ref.starts_with(provider)) {
            candidate.score += 1.0;
        }
        by_ref
            .entry(candidate.source_ref.clone())
            .and_modify(|old: &mut retrieval::Retrieved| {
                if candidate.score > old.score {
                    *old = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let mut results = by_ref.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(8);

    // Une seconde vague sémantique couvre les formulations conceptuelles. Les
    // deux index sont interrogés, toujours hors du chemin de réponse.
    let semantic_db = core.db.clone();
    let semantic_llm = core.llm.clone();
    let semantic_bus = core.bus.clone();
    let semantic_query = target.clone();
    let semantic_session = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let (local, cloud) = tokio::join!(
            retrieval::search_source(&semantic_db, &semantic_llm, &semantic_query, 8, "files"),
            retrieval::search_source(&semantic_db, &semantic_llm, &semantic_query, 8, "cloud"),
        );
        let mut streamed = local.unwrap_or_default();
        streamed.extend(cloud.unwrap_or_default());
        if let Some(provider) = preferred_provider {
            for result in &mut streamed {
                if result.source_ref.starts_with(provider) {
                    result.score += 1.0;
                }
            }
        }
        streamed.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        streamed.truncate(8);
        semantic_bus.emit(BusEvent::SemanticResults {
            session_id: semantic_session,
            results: streamed,
        });
    });

    crate::security::log_access(&core.db, "federated", "search", Some(&target));
    let correction = if is_correction {
        if formal {
            "Vous avez raison : j’ai relancé la recherche dans toutes vos sources disponibles.\n\n"
        } else {
            "Tu as raison : j’ai relancé la recherche dans toutes tes sources disponibles.\n\n"
        }
    } else {
        ""
    };
    let text = if results.is_empty() {
        format!(
            "{correction}Je n’ai trouvé aucun document suffisamment pertinent pour « {target} » sur le Mac, Google Drive ou OneDrive. Les connecteurs absents ou déconnectés ont été ignorés, sans générer de faux résultat."
        )
    } else {
        let mut body = format!(
            "{correction}J’ai cherché sur le Mac, Google Drive et OneDrive et trouvé {} document{} pertinent{} :\n",
            results.len(),
            if results.len() > 1 { "s" } else { "" },
            if results.len() > 1 { "s" } else { "" },
        );
        for (index, result) in results.iter().enumerate() {
            let location = if result.source_ref.starts_with("google:drive:") {
                "Google Drive"
            } else if result.source_ref.starts_with("microsoft:drive:") {
                "OneDrive"
            } else {
                "Mac"
            };
            let path = result.path.as_deref().unwrap_or(&result.source_ref);
            body.push_str(&format!(
                "\n{}. **{}** — {} — {}",
                index + 1,
                result.title,
                location,
                path
            ));
        }
        body.push_str(if formal {
            "\n\nCliquez sur le nom du document pour l’ouvrir."
        } else {
            "\n\nClique sur le nom du document pour l’ouvrir."
        });
        body
    };
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "complete",
        if results.is_empty() {
            "Recherche fédérée terminée sans résultat"
        } else {
            "Recherche fédérée terminée"
        },
        Some("Mac · Google Drive · OneDrive".into()),
        5,
        5,
        "done",
    );
    Ok(Answer {
        text,
        sources: results,
        pending_actions: Vec::new(),
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Retrouver un message REÇU, dans les messageries connectées.
///
/// Rien de tout cela ne passait auparavant : « retrouve le mail de Liverpool »
/// était compris comme une recherche de DOCUMENT, et l'index de fichiers
/// répondait par des images dont le nom contenait « mail » ou « maillot ». La
/// capacité existait pourtant — Gmail et Graph savent chercher côté serveur, et
/// renvoient un lien direct vers le message.
///
/// On n'indexe pas des milliers de mails pour cela : la recherche est faite
/// PAR le fournisseur, à la demande. Seuls les résultats affichés sont retenus,
/// pour que leur lien soit ouvrable et qu'ils restent consultables hors ligne.
async fn answer_mail_search(
    core: &Core,
    session_id: &str,
    query: &str,
    scope: intent::Scope,
    action: intent::MailAction,
    formal: bool,
) -> Result<Answer> {
    let comptes = crate::connectors::mail::available_channels(&core.db);
    let boites: Vec<&'static str> = comptes
        .iter()
        .filter(|(id, _)| *id != "apple")
        .map(|(id, _)| *id)
        .collect();
    let apple_indexe = crate::connectors::mail::indexed_count(&core.db).unwrap_or(0) > 0;

    // Franchise d'abord : sans messagerie connectée, on le dit, on ne cherche
    // pas pour la forme.
    if boites.is_empty() && !apple_indexe {
        let text = if formal {
            "Désolé, pour l'instant je ne peux pas chercher dans vos messageries : aucun compte mail n'est connecté. Vous pouvez en ajouter un dans Connecteurs. Si vous avez besoin de moi pour autre chose, n'hésitez pas !"
        } else {
            "Désolé, pour l'instant je ne peux pas chercher dans tes messageries : aucun compte mail n'est connecté. Tu peux en ajouter un dans Connecteurs. Si tu as besoin de moi pour autre chose, n'hésite pas !"
        }
        .to_string();
        memory::persist_turn(&core.db, session_id, "assistant", &text)?;
        return Ok(Answer {
            text,
            sources: vec![],
            pending_actions: vec![],
            choices: vec![],
            session_id: session_id.into(),
            degraded: false,
        });
    }

    emit_progress(
        core,
        session_id,
        "retrieve",
        "Recherche dans les messageries",
        Some(mail_boxes_label(&boites, apple_indexe)),
        3,
        5,
        "running",
    );

    // Ce qui est déjà indexé localement (Apple Mail, mails déjà vus).
    let mut results = retrieval::search_lexical_source(&core.db, query, 8, "mail").await?;

    // Puis chez les fournisseurs, en direct. Un compte lent ne fait pas échouer
    // la réponse : les autres résultats restent affichables.
    // Une messagerie nommée est une PRÉFÉRENCE, jamais une exclusion : la
    // compréhension d'intention devine parfois « google » sur un simple mot, et
    // un message rangé dans l'autre boîte deviendrait introuvable.
    let mut demandes: Vec<&'static str> = boites.clone();
    match scope {
        intent::Scope::Google => demandes.sort_by_key(|p| *p != "google"),
        intent::Scope::Microsoft => demandes.sort_by_key(|p| *p != "microsoft"),
        _ => {}
    }
    for provider in demandes.iter() {
        let live = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            crate::connectors::external::live_search_provider("mail", query, provider),
        )
        .await;
        let Ok(Ok(values)) = live else { continue };
        for value in values {
            let Some(source_ref) = value["source_ref"].as_str() else {
                continue;
            };
            remember_live_cloud(core, &value).await;
            if results.iter().any(|result| result.source_ref == source_ref) {
                continue;
            }
            if let Some(result) = retrieved_from_live_mail(&value) {
                results.push(result);
            }
        }
    }
    results.truncate(8);

    // Afficher ou supprimer suppose UN message. Tant qu'il y a une ambiguïté, on
    // montre la liste et on laisse l'utilisateur désigner : agir sur « le
    // premier résultat » serait deviner à sa place.
    let geste_unitaire = matches!(
        action,
        intent::MailAction::Afficher | intent::MailAction::Supprimer
    );
    if geste_unitaire && results.len() == 1 {
        return match action {
            intent::MailAction::Afficher => {
                answer_mail_open(core, session_id, &results[0], formal).await
            }
            _ => propose_mail_deletion(core, session_id, &results[0], formal),
        };
    }

    let text = if results.is_empty() {
        let ou = mail_boxes_label(&boites, apple_indexe);
        if formal {
            format!("Je n'ai trouvé aucun message correspondant dans {ou}. Précisez-moi un expéditeur, un mot de l'objet ou une période, et je relance la recherche.")
        } else {
            format!("Je n'ai trouvé aucun message correspondant dans {ou}. Donne-moi un expéditeur, un mot de l'objet ou une période, et je relance la recherche.")
        }
    } else if geste_unitaire {
        let mut body = format!(
            "{} messages correspondent. Lequel {} ?",
            results.len(),
            if action == intent::MailAction::Supprimer {
                "faut-il mettre à la corbeille"
            } else if formal {
                "voulez-vous lire"
            } else {
                "veux-tu lire"
            }
        );
        for (index, result) in results.iter().enumerate() {
            body.push_str(&format!(
                "\n{}. **{}** — {}",
                index + 1,
                result.title,
                mail_origin(result)
            ));
        }
        body
    } else {
        let mut body = format!(
            "J'ai trouvé {} message{} dans {} :",
            results.len(),
            if results.len() > 1 { "s" } else { "" },
            mail_boxes_label(&boites, apple_indexe)
        );
        for (index, result) in results.iter().enumerate() {
            body.push_str(&format!(
                "\n{}. **{}** — {}",
                index + 1,
                result.title,
                mail_origin(result)
            ));
        }
        body.push_str(if formal {
            "\n\nCliquez sur l'objet pour ouvrir le message."
        } else {
            "\n\nClique sur l'objet pour ouvrir le message."
        });
        body
    };
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "complete",
        if results.is_empty() {
            "Recherche de messages terminée sans résultat"
        } else {
            "Recherche de messages terminée"
        },
        Some(mail_boxes_label(&boites, apple_indexe)),
        5,
        5,
        "done",
    );
    Ok(Answer {
        text,
        sources: results,
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Voir sa boîte : les derniers messages, ou seulement les non lus.
///
/// Ce n'est pas une recherche — il n'y a rien à chercher. Passer par la
/// recherche obligeait à inventer des mots-clés à partir d'une phrase qui n'en
/// contient pas, et le fournisseur répondait à côté.
async fn answer_mail_list(
    core: &Core,
    session_id: &str,
    user_text: &str,
    formal: bool,
) -> Result<Answer> {
    let boites: Vec<&'static str> = crate::connectors::mail::available_channels(&core.db)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| *id != "apple")
        .collect();
    if boites.is_empty() {
        return no_mailbox_answer(core, session_id, formal);
    }
    let non_lus = ["non lu", "non-lu", "pas lu", "unread"]
        .iter()
        .any(|term| crate::db::fold(user_text).contains(term));
    emit_progress(
        core,
        session_id,
        "retrieve",
        if non_lus {
            "Lecture des messages non lus"
        } else {
            "Lecture des derniers messages"
        },
        None,
        3,
        5,
        "running",
    );
    let mut results = Vec::new();
    for provider in &boites {
        let live = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            crate::connectors::external::list_mail(provider, non_lus, 8),
        )
        .await;
        let Ok(Ok(messages)) = live else { continue };
        for value in messages {
            remember_live_cloud(core, &value).await;
            if let Some(result) = retrieved_from_live_mail(&value) {
                results.push(result);
            }
        }
    }
    results.truncate(10);

    let text = if results.is_empty() {
        if non_lus {
            "Aucun message non lu.".to_string()
        } else {
            "Aucun message récent dans les boîtes connectées.".to_string()
        }
    } else {
        let mut body = format!(
            "{} dans {} :",
            if non_lus {
                "Messages non lus"
            } else {
                "Derniers messages reçus"
            },
            mail_boxes_label(&boites, false)
        );
        for (index, result) in results.iter().enumerate() {
            body.push_str(&format!(
                "\n{}. **{}** — {}",
                index + 1,
                result.title,
                mail_origin(result)
            ));
        }
        body.push_str(if formal {
            "\n\nCliquez sur l'objet pour ouvrir le message."
        } else {
            "\n\nClique sur l'objet pour ouvrir le message."
        });
        body
    };
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "complete",
        "Boîte consultée",
        None,
        5,
        5,
        "done",
    );
    Ok(Answer {
        text,
        sources: results,
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Affiche le contenu d'un message dans le fil, avec son lien.
async fn answer_mail_open(
    core: &Core,
    session_id: &str,
    message: &retrieval::Retrieved,
    formal: bool,
) -> Result<Answer> {
    let ctx = crate::tools::ToolCtx {
        db: core.db.clone(),
        llm: core.llm.clone(),
        bus: core.bus.clone(),
        settings: crate::settings::load(&core.db)?,
    };
    let ouvert = crate::tools::execute(
        &ctx,
        "mail.open",
        &json!({ "source_ref": message.source_ref }),
    )
    .await;
    let invite = if formal {
        "Cliquez sur l'objet pour l'ouvrir dans votre messagerie."
    } else {
        "Clique sur l'objet pour l'ouvrir dans ta messagerie."
    };
    let text = match ouvert {
        Ok(outcome) => {
            let corps = outcome.result["body"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .chars()
                .take(2_000)
                .collect::<String>();
            let entete = format!("**{}** — {}", message.title, mail_origin(message));
            if corps.is_empty() {
                format!("{entete}\n\nJe n'ai pas pu lire le corps de ce message. {invite}")
            } else {
                let cite = corps
                    .lines()
                    .map(|line| format!("> {line}").trim_end().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{entete}\n\n{cite}\n\n{invite}")
            }
        }
        Err(error) => format!("Je n'ai pas pu ouvrir ce message : {error}"),
    };
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    Ok(Answer {
        text,
        sources: vec![message.clone()],
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Prépare la mise à la corbeille — et s'arrête là. Effacer une donnée de
/// l'utilisateur passe toujours par sa validation explicite, quel que soit son
/// niveau d'autonomie.
fn propose_mail_deletion(
    core: &Core,
    session_id: &str,
    message: &retrieval::Retrieved,
    formal: bool,
) -> Result<Answer> {
    let args = json!({ "source_ref": message.source_ref });
    let risk = actions::classify("mail.delete", &args);
    let preview = format!(
        "Mettre à la corbeille « {} » — {}",
        message.title,
        mail_origin(message)
    );
    let action_id = actions::queue_pending(
        &core.db,
        "mail.delete",
        &args,
        risk,
        &preview,
        false,
        Some(session_id),
    )?;
    core.bus.emit(BusEvent::ActionAwaitingConfirmation {
        action_id: action_id.clone(),
        tool: "mail.delete".into(),
        preview: preview.clone(),
        risk_class: risk.as_str().into(),
    });
    let text = format!(
        "Je peux mettre « {} » à la corbeille de {} messagerie. Le message y restera récupérable.",
        message.title,
        if formal { "votre" } else { "ta" }
    );
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    Ok(Answer {
        text,
        sources: vec![message.clone()],
        pending_actions: vec![PendingRef {
            action_id,
            tool: "mail.delete".into(),
            preview,
            risk_class: risk.as_str().into(),
        }],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Aucune messagerie connectée : Syn le dit, il ne cherche pas pour la forme.
fn no_mailbox_answer(core: &Core, session_id: &str, formal: bool) -> Result<Answer> {
    let text = if formal {
        "Désolé, pour l'instant je ne peux pas consulter vos messageries : aucun compte mail n'est connecté. Vous pouvez en ajouter un dans Connecteurs."
    } else {
        "Désolé, pour l'instant je ne peux pas consulter tes messageries : aucun compte mail n'est connecté. Tu peux en ajouter un dans Connecteurs."
    }
    .to_string();
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    Ok(Answer {
        text,
        sources: vec![],
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Où Syn a cherché, dit avec les noms que l'utilisateur connaît.
fn mail_boxes_label(boites: &[&'static str], apple: bool) -> String {
    let mut noms: Vec<&str> = Vec::new();
    if apple {
        noms.push("Apple Mail");
    }
    for boite in boites {
        noms.push(crate::connectors::mail::channel_label(boite));
    }
    match noms.len() {
        0 => "vos messageries".to_string(),
        1 => noms[0].to_string(),
        _ => {
            let dernier = noms.pop().unwrap();
            format!("{} et {dernier}", noms.join(", "))
        }
    }
}

/// L'expéditeur et la boîte d'un message, pour que la liste se lise sans avoir
/// à ouvrir quoi que ce soit.
fn mail_origin(result: &retrieval::Retrieved) -> String {
    let boite = if result.source_ref.starts_with("microsoft:") {
        "Outlook"
    } else if result.source_ref.starts_with("google:") {
        "Gmail"
    } else {
        "Apple Mail"
    };
    let expediteur = result
        .snippet
        .lines()
        .find_map(|line| line.strip_prefix("De : "))
        .map(str::trim)
        .filter(|from| !from.is_empty());
    match expediteur {
        Some(from) => format!("{from} · {boite}"),
        None => boite.to_string(),
    }
}

fn retrieved_from_live_mail(value: &serde_json::Value) -> Option<retrieval::Retrieved> {
    let source_ref = value["source_ref"].as_str()?;
    let expediteur = value["from"].as_str().unwrap_or_default().trim();
    let snippet = value["snippet"].as_str().unwrap_or_default();
    Some(retrieval::Retrieved {
        item_id: value["item_id"].as_str().unwrap_or(source_ref).to_string(),
        source: "mail".into(),
        source_ref: source_ref.to_string(),
        title: value["title"].as_str().unwrap_or("Message").to_string(),
        path: value["path"].as_str().map(str::to_string),
        snippet: if expediteur.is_empty() {
            snippet.to_string()
        } else {
            format!("De : {expediteur}\n{snippet}")
        },
        score: value["score"].as_f64().unwrap_or(5.0) as f32,
    })
}

async fn answer_cloud_file_search(
    core: &Core,
    session_id: &str,
    query: &str,
    provider: Option<&'static str>,
    is_correction: bool,
    formal: bool,
) -> Result<Answer> {
    // `query` est déjà le sujet compris ; le re-découper reviendrait à faire
    // confiance à la forme de la phrase après l'avoir justement abandonnée.
    let target = query.to_string();
    let mut results = retrieval::search_lexical_source(&core.db, &target, 12, "cloud").await?;
    if let Some(provider) = provider {
        let prefix = format!("{provider}:drive:");
        results.retain(|result| result.source_ref.starts_with(&prefix));
    }

    // Un fournisseur lent ne doit pas faire échouer la réponse : le cache local
    // reste affichable, et le dépassement devient un diagnostic dans le texte.
    let live = match provider {
        Some(provider) => tokio::time::timeout(
            std::time::Duration::from_secs(15),
            crate::connectors::external::live_search_provider("cloud", &target, provider),
        )
        .await
        .unwrap_or_else(|_| {
            Err(crate::error::AppError::Other(
                "la recherche cloud a dépassé 15 secondes".into(),
            ))
        }),
        None => Ok(crate::connectors::external::live_search("cloud", &target).await),
    };
    let live_error = live.as_ref().err().map(ToString::to_string);
    if let Ok(values) = live {
        for value in values {
            let Some(source_ref) = value["source_ref"].as_str() else {
                continue;
            };
            remember_live_cloud(core, &value).await;
            if results.iter().any(|result| result.source_ref == source_ref) {
                continue;
            }
            if let Some(result) = retrieved_from_live_cloud(&value) {
                results.push(result);
            }
        }
    }
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(8);

    // Le cache sémantique complète ensuite la réponse, mais reste strictement
    // limité au fournisseur demandé : aucun fichier local ne peut s'y glisser.
    let semantic_db = core.db.clone();
    let semantic_llm = core.llm.clone();
    let semantic_bus = core.bus.clone();
    let semantic_query = target.clone();
    let semantic_session = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        if let Ok(Ok(mut semantic)) = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            retrieval::search_source(&semantic_db, &semantic_llm, &semantic_query, 8, "cloud"),
        )
        .await
        {
            if let Some(provider) = provider {
                let prefix = format!("{provider}:drive:");
                semantic.retain(|result| result.source_ref.starts_with(&prefix));
            }
            semantic_bus.emit(BusEvent::SemanticResults {
                session_id: semantic_session,
                results: semantic,
            });
        }
    });

    crate::security::log_access(
        &core.db,
        provider.unwrap_or("cloud"),
        "live_search",
        Some(&target),
    );
    let correction = if is_correction {
        if formal {
            "Vous avez raison : j’ai limité cette nouvelle recherche au connecteur demandé.\n\n"
        } else {
            "Tu as raison : j’ai limité cette nouvelle recherche au connecteur demandé.\n\n"
        }
    } else {
        ""
    };
    let service = match provider {
        Some("google") => "Google Drive",
        Some("microsoft") => "OneDrive",
        _ => "vos services cloud",
    };
    let text = if results.is_empty() {
        let diagnostic = live_error
            .map(|error| format!(" La recherche directe a échoué : {error}"))
            .unwrap_or_default();
        format!(
            "{correction}Je n’ai trouvé aucun document correspondant à « {target} » dans {service}.{diagnostic} Je n’affiche pas de fichiers locaux, car ils ne font pas partie du périmètre demandé."
        )
    } else {
        let mut body = format!(
            "{correction}J’ai trouvé {} document{} dans {service} :\n",
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
            "\n\nCliquez sur le nom du document pour l’ouvrir."
        } else {
            "\n\nClique sur le nom du document pour l’ouvrir."
        });
        body
    };
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "complete",
        if results.is_empty() {
            "Recherche cloud terminée sans résultat"
        } else {
            "Documents cloud pertinents trouvés"
        },
        Some(format!("Source : {service}")),
        5,
        5,
        "done",
    );
    Ok(Answer {
        text,
        sources: results,
        pending_actions: Vec::new(),
        choices: vec![],
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
    // Une confirmation est une réponse BRÈVE. « Tu peux envoyer un courriel à
    // Julie pour lui dire que je serai en retard » commence comme un accord et
    // se termine en demande NOUVELLE : la prendre pour une confirmation ferait
    // partir le mail précédent à la place.
    if words.len() > 10 {
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

/// Exécute un envoi déjà préparé, quand l'utilisateur vient d'y consentir dans
/// le fil plutôt qu'en cliquant la carte.
///
/// Le CONSENTEMENT est compris en amont (`Step::SendConfirmation`) ; cette
/// fonction ne juge plus rien, elle agit — et seulement s'il n'y a aucune
/// ambiguïté sur l'action visée.
async fn confirm_pending_mail_from_chat(
    core: &Core,
    session_id: &str,
    settings: &crate::settings::Settings,
) -> Result<Option<Answer>> {
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
            // L'envoi est fait : l'état de composition ne doit pas survivre,
            // sinon le mail suivant hérite du destinataire de celui-ci.
            crate::connectors::mail::clear_composition(&core.db, session_id)?;
            mail_flow::note(
                core,
                session_id,
                settings
                    .voice
                    .pick("Tu as confirmé l'envoi", "Vous avez confirmé l'envoi"),
            )?;
            let text = crate::tools::outcome_summary(
                "mail.send",
                &outcome.result,
                settings.voice.vouvoie(),
            );
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
                choices: vec![],
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

/// L'utilisateur a-t-il dit de quoi le mail doit parler ?
///
/// Trois signaux, du plus sûr au plus souple : une demande d'envoi qui porte
/// déjà son contenu, une suite de conversation qui le donne (« dis-lui que… »),
/// ou un sujet extrait par la compréhension d'intention. Une simple
/// confirmation d'adresse, elle, ne dit rien du message — la prendre pour un
/// contenu ferait rédiger un mail sur « c'est correct ».
fn mail_content_expressed(user_text: &str, understood: &intent::Intent) -> bool {
    if is_explicit_chat_confirmation(user_text) || mail_flow::is_draft_approval(user_text) {
        return false;
    }
    if is_mail_composition_query(user_text) && !mail_request_missing_content(user_text) {
        return true;
    }
    if is_mail_content_followup(user_text) {
        return true;
    }
    understood
        .subject
        .as_deref()
        .is_some_and(|subject| subject.split_whitespace().count() >= 2)
}

/// La réponse affirme-t-elle qu'un mail est parti ?
fn claims_a_sent_mail(text: &str) -> bool {
    let text = crate::db::fold(text);
    [
        "mail a ete envoye",
        "email a ete envoye",
        "message a ete envoye",
        "mail est envoye",
        "mail envoye a",
        "j'ai envoye le mail",
        "j'ai envoye ton mail",
        "j'ai bien envoye",
        "je l'ai envoye",
        "c'est envoye",
    ]
    .iter()
    .any(|claim| text.contains(claim))
}

/// Un envoi RÉEL, tracé dans le journal des actions de cette conversation.
/// C'est le journal qui fait foi, jamais la phrase du modèle.
fn mail_really_sent(db: &crate::db::Db, session_id: &str) -> Result<bool> {
    Ok(actions::list_actions(db, Some("executed"), 30)?
        .iter()
        .any(|action| {
            action.tool == "mail.send"
                && action.session_id.as_deref() == Some(session_id)
                && action.created_at >= crate::db::now() - 3600
        }))
}

/// L'utilisateur a-t-il demandé un BROUILLON, et non un envoi ? Sans cette
/// distinction, « rédige un mail… » produisait un brouillon silencieux dans
/// Mail au lieu du texte à relire.
fn asked_for_draft(text: &str) -> bool {
    let text = crate::db::fold(text);
    [
        "brouillon",
        "draft",
        "sans l'envoyer",
        "ne l'envoie pas",
        "ne pas l'envoyer",
    ]
    .iter()
    .any(|term| text.contains(term))
}

/// Secours déterministe pour l'action, quand le modèle n'a rien dit. Comme
/// toute reconnaissance par mots, elle ne vaut que pour les formulations les
/// plus explicites — et cesse de servir dès que la compréhension répond.
fn mail_action_fallback(text: &str) -> intent::MailAction {
    let folded = crate::db::fold(text);
    if ["supprime", "efface", "vire", "jette", "corbeille", "delete"]
        .iter()
        .any(|term| folded.contains(term))
    {
        return intent::MailAction::Supprimer;
    }
    if ["ouvre", "affiche", "montre-moi le", "lis-moi", "contenu du"]
        .iter()
        .any(|term| folded.contains(term))
    {
        return intent::MailAction::Afficher;
    }
    if [
        "liste",
        "derniers",
        "non lus",
        "non-lus",
        "ma boite",
        "recu aujourd'hui",
    ]
    .iter()
    .any(|term| folded.contains(term))
    {
        return intent::MailAction::Lister;
    }
    intent::MailAction::Retrouver
}

/// Secours déterministe : chercher un message REÇU, quand le modèle local est
/// arrêté. Le vrai aiguillage est la compréhension d'intention — ceci ne sert
/// qu'à ne pas rester muet hors ligne.
fn is_mail_search_query(text: &str) -> bool {
    let folded = crate::db::fold(text);
    let mentions_mail = ["mail", "email", "courriel", "message"]
        .iter()
        .any(|term| folded.contains(term));
    let retrouve = [
        "retrouve",
        "retrouver",
        "cherche",
        "chercher",
        "recherche",
        "ou est",
        "j'ai recu",
        "recu de",
        "montre",
        "affiche",
    ]
    .iter()
    .any(|term| folded.contains(term));
    // « envoie un mail » et « réponds à ce mail » composent : ils ne cherchent pas.
    mentions_mail && retrouve && !is_mail_composition_query(text)
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
    // `content_already_given` : un contenu a-t-il DÉJÀ été donné par
    // l'utilisateur au fil de la conversation ? Si oui, la garde contre un
    // contenu inventé par le modèle n'a plus lieu de s'appliquer — elle ferait
    // redemander une information acquise, l'amnésie constatée le 17/08.
    content_already_given: bool,
    // `recipient_from_address_book` : l'adresse a-t-elle été trouvée par Syn
    // dans le carnet de l'utilisateur (people.resolve_email) ? Elle est alors
    // légitime par construction. Sans ce chemin, le parcours des maquettes —
    // Syn trouve l'adresse, l'utilisateur la confirme — était refusé par le
    // contrôle anti-adresse-inventée, et l'envoi restait bloqué en silence.
    recipient_from_address_book: bool,
) -> Option<Value> {
    let to = args["to"].as_str().unwrap_or("").trim();
    let subject = args["subject"].as_str().unwrap_or("").trim();
    let body = args["body"].as_str().unwrap_or("").trim();

    if subject.is_empty()
        || body.is_empty()
        || (!content_already_given && mail_request_missing_content(current_user_text))
    {
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
    // Ordre volontaire : la LÉGITIMITÉ du destinataire se vérifie avant le
    // confort du choix de compte. Un compte manquant ne doit jamais masquer une
    // adresse inventée — l'inverse laisserait passer le contrôle de sécurité
    // dès qu'aucun connecteur n'est configuré.
    let history_folded = crate::db::fold(trusted_user_history);
    let recipient_is_legitimate = recipient_from_address_book
        || history_folded.contains(&crate::db::fold(to))
        || matches!(
            crate::connectors::people::email_is_known_for_mentioned_person(
                db,
                to,
                trusted_user_history,
            ),
            Ok(true)
        );
    if !recipient_is_legitimate {
        return Some(json!({
            "status": "destinataire_non_resolu",
            "rejected_address": to,
            "note": "Cette adresse n'a été ni donnée par l'utilisateur ni résolue depuis le contact nommé. Ne l'invente pas : appelle people.resolve_email ou demande l'adresse."
        }));
    }

    // Par quel compte ? Le défaut « Apple Mail » était choisi sans vérifier
    // qu'il fonctionne : sur une application non signée, l'envoi échouait alors
    // que Gmail et Outlook étaient connectés. Le choix de l'expéditeur revient à
    // l'utilisateur dès qu'il en a plusieurs.
    let channels = crate::connectors::mail::available_channels(db);
    let requested = args["via"].as_str().unwrap_or("").trim();
    if channels.is_empty() {
        return Some(json!({
            "status": "aucun_compte_denvoi",
            "note": "Aucun compte ne peut envoyer de mail. Propose d'enregistrer un brouillon avec mail.draft, et invite l'utilisateur à connecter Google ou Microsoft dans Connecteurs."
        }));
    }
    if !requested.is_empty() && !channels.iter().any(|(id, _)| *id == requested) {
        return Some(json!({
            "status": "compte_indisponible",
            "demande": requested,
            "comptes_disponibles": channels.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "note": "Ce compte n'est pas disponible. Demande à l'utilisateur lequel utiliser parmi les comptes disponibles."
        }));
    }
    if requested.is_empty() && channels.len() > 1 {
        return Some(json!({
            "status": "compte_a_choisir",
            "comptes_disponibles": channels
                .iter()
                .map(|(id, label)| json!({"via": id, "libelle": label}))
                .collect::<Vec<_>>(),
            "note": "Plusieurs comptes peuvent envoyer ce mail. Demande à l'utilisateur lequel utiliser, puis rappelle mail.send avec le champ `via` renseigné. N'envoie rien avant sa réponse."
        }));
    }
    None
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
    !explicit_content && !quoted_or_separated && !states_an_intent(&text)
}

/// « Un mail très concis pour lui souhaiter un bon anniversaire » ne contient
/// pas le texte du mail, mais dit largement de quoi il parle : Syn a de quoi
/// rédiger une proposition. Redemander « que veux-tu dire ? » à ce moment-là
/// était une boucle — l'utilisateur venait de répondre.
fn states_an_intent(folded: &str) -> bool {
    for lead in [
        " pour ",
        " afin de ",
        " au sujet de ",
        " a propos de ",
        " concernant ",
    ] {
        let Some((_, tail)) = folded.split_once(lead) else {
            continue;
        };
        let words: Vec<&str> = tail.split_whitespace().collect();
        // « pour moi », « pour ce soir » : une précision de circonstance, pas
        // une intention de message.
        let first_is_pronoun = words.first().is_some_and(|word| {
            [
                "moi",
                "toi",
                "nous",
                "vous",
                "eux",
                "ca",
                "cela",
                "ce",
                "cet",
                "cette",
                "demain",
                "aujourd'hui",
                "hier",
            ]
            .contains(word)
        });
        if !first_is_pronoun && words.len() >= 2 {
            return true;
        }
    }
    false
}

/// Un contact à retenir après un envoi : l'adresse utilisée n'est encore liée à
/// personne, et un nom a été cherché sans succès dans les dernières minutes.
///
/// Aucune phrase n'est interprétée : le nom vient de l'argument STRUCTURÉ passé
/// à `people.resolve_email`, l'adresse de celui de `mail.send`. C'est ce qui
/// rend le déclenchement fiable plutôt que devinatoire.
fn learnable_contact(db: &crate::db::Db, args: &Value) -> Result<Option<(String, String)>> {
    let email = args["to"].as_str().unwrap_or("").trim().to_lowercase();
    if !(email.contains('@') && email.contains('.')) {
        return Ok(None);
    }
    // Déjà connue : il n'y a rien à apprendre.
    let known: bool = db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM people
             WHERE syn_fold(COALESCE(comm_channels,'')) LIKE '%'||?1||'%' LIMIT 1",
            rusqlite::params![email],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;
    if known {
        return Ok(None);
    }
    // Le nom que l'utilisateur cherchait, s'il y en a eu un récemment.
    let name: Option<String> = db.read(|c| {
        Ok(c.query_row(
            "SELECT item_ref FROM access_log
             WHERE connector='people' AND operation='resolve_email_unresolved'
               AND item_ref IS NOT NULL AND created_at >= ?1
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![crate::db::now() - 1800],
            |row| row.get::<_, String>(0),
        )
        .ok())
    })?;
    // Sans nom, on ne propose rien : une fiche « paulpro » créée à partir d'une
    // adresse serait un déchet de plus dans la mémoire.
    Ok(name
        .map(|name| name.trim().to_string())
        .filter(|name| name.len() >= 2)
        .map(|name| (name, email)))
}

/// Retire d'une réponse ce qui appartient à la mécanique interne : blocs JSON
/// recopiés d'un résultat d'outil, et identifiants techniques.
///
/// La consigne au modèle l'interdit déjà, mais une consigne n'est pas une
/// garantie : l'utilisateur a vu `{"matches":[{"email":…}]}` et un `action_id`
/// s'afficher au milieu d'une phrase. Ce filet est déterministe.
fn strip_internal_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        // Un objet équilibré qui contient une clé entre guillemets est une
        // structure, pas une tournure de phrase.
        match balanced_span(&rest[start..]) {
            Some(len) if rest[start..start + len].contains("\":") => {
                out.push_str(&rest[..start]);
                rest = &rest[start + len..];
            }
            _ => {
                out.push_str(&rest[..start + 1]);
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);

    // Identifiants techniques (UUID) : ils n'ont aucun sens pour l'utilisateur.
    out.lines()
        .map(|line| {
            line.split_whitespace()
                .filter(|word| !is_technical_identifier(word))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn is_technical_identifier(word: &str) -> bool {
    let bare = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    bare.len() == 36
        && bare.chars().filter(|c| *c == '-').count() == 4
        && bare.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Longueur du span `{…}` équilibré au début de `text`, guillemets pris en compte.
fn balanced_span(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in text.char_indices() {
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
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
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
        file_matches_requested_domain, file_search_scope, is_device_diagnostic_query,
        is_explicit_chat_confirmation, is_file_search_query, mail_request_missing_content,
        mail_send_preflight, requested_document_query, resolve_file_search_request,
        screen_context_text, FileSearchScope,
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
    fn google_docs_reste_un_perimetre_cloud_strict() {
        let query = "Tu peux me ressortir le document du Jeu de la Vie qui se trouve dans mes Google Docs ?";
        assert_eq!(
            file_search_scope(query),
            FileSearchScope::Cloud(Some("google"))
        );
        assert_eq!(requested_document_query(query), "Jeu de la Vie");
        assert_eq!(
            file_search_scope("Retrouve mon contrat dans OneDrive"),
            FileSearchScope::Cloud(Some("microsoft"))
        );
        assert_eq!(
            file_search_scope("Retrouve mon contrat sur le disque"),
            FileSearchScope::Local
        );
        assert_eq!(
            file_search_scope("Retrouve mon contrat de location"),
            FileSearchScope::Federated(None)
        );
        assert_eq!(
            file_search_scope("Retrouve le document Word du budget"),
            FileSearchScope::Federated(Some("microsoft"))
        );
        // L'expression est rendue telle que l'utilisateur l'a écrite, liaisons
        // internes comprises : « contrat de location » peut être le titre exact
        // d'un fichier, « contrat location » ne l'est jamais.
        assert_eq!(
            requested_document_query("Peux-tu retrouver mon contrat de location ?"),
            "contrat de location"
        );
    }

    /// Aucune de ces demandes n'est prévue par le code : elles n'ont ni verbe,
    /// ni tournure, ni langue en commun. C'est la garde contre une correction
    /// qui ne vaudrait que pour l'exemple ayant servi à la trouver.
    #[test]
    fn le_sujet_est_extrait_sans_connaitre_la_tournure() {
        for (demande, sujet) in [
            (
                "Tu peux me ressortir le document du Jeu de la Vie qui se trouve dans mes Google Docs ?",
                "Jeu de la Vie",
            ),
            (
                "cherche le rapport de stage de Maxime dans mon OneDrive",
                "rapport de stage de Maxime",
            ),
            (
                "montre-moi le tableur du budget prévisionnel 2027",
                "tableur du budget prévisionnel 2027",
            ),
            ("Where is the Q3 revenue forecast?", "Q3 revenue forecast"),
            (
                "ouvre le vade_mecum des sections européennes",
                "vade_mecum des sections européennes",
            ),
            (
                "il me faudrait la convention collective Syntec, stp",
                "convention collective Syntec",
            ),
            ("Cours 2", "Cours 2"),
        ] {
            assert_eq!(requested_document_query(demande), sujet, "« {demande} »");
        }

        // Une demande sans aucun mot porteur ne doit pas fabriquer un sujet.
        assert_eq!(requested_document_query("ouvre-le stp"), "ouvre-le stp");
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
        assert!(is_file_search_query("Ressors-moi le Word du budget 2026"));
        assert!(is_file_search_query(
            "Ouvre la présentation PowerPoint du comité"
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
        // Piège : une demande NEUVE qui commence par une tournure d'accord.
        // Confirmée par erreur, elle enverrait le mail précédent.
        assert!(!is_explicit_chat_confirmation(
            "Tu peux envoyer un courriel à Julie pour lui dire que je serai en retard ?"
        ));
    }

    /// Deux comptes connectés : Syn ne doit pas choisir à la place de
    /// l'utilisateur. C'est le cas réel du 17/08 — Apple Mail indisponible sur
    /// une application non signée, Gmail et Outlook prêts, et Syn qui échouait
    /// silencieusement au lieu de demander.
    #[test]
    fn avec_plusieurs_comptes_syn_demande_lequel_utiliser() {
        let dir = std::env::temp_dir().join(format!("syn-mail-via-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"2".repeat(64)).unwrap();
        crate::connectors::set_status(&db, "google", "google", "connected").unwrap();
        crate::connectors::set_status(&db, "microsoft", "microsoft", "connected").unwrap();

        let user = "Envoie un mail à paul@exemple.fr qui dit bonjour";
        let args = serde_json::json!({
            "to": "paul@exemple.fr", "subject": "Bonjour", "body": "Bonjour."
        });
        let observation = mail_send_preflight(&db, &args, user, user, true, false).unwrap();
        assert_eq!(observation["status"], "compte_a_choisir");
        let comptes = observation["comptes_disponibles"].as_array().unwrap();
        assert!(comptes.len() >= 2, "{observation}");

        // Une fois le compte précisé, plus rien ne bloque.
        let mut choisi = args.clone();
        choisi["via"] = serde_json::json!("google");
        assert!(mail_send_preflight(&db, &choisi, user, user, true, false).is_none());

        // Un compte non connecté est refusé explicitement, pas silencieusement.
        let mut absent = args.clone();
        absent["via"] = serde_json::json!("slack");
        assert_eq!(
            mail_send_preflight(&db, &absent, user, user, true, false).unwrap()["status"],
            "compte_indisponible"
        );
        let _ = std::fs::remove_dir_all(dir);
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
        let rejected = mail_send_preflight(&db, &args, user, user, true, false).unwrap();
        assert_eq!(rejected["status"], "destinataire_non_resolu");

        crate::memory::find_or_create_person(&db, "Paul", Some("paul@exemple.fr"), None).unwrap();
        // Un destinataire légitime ne suffit pas : encore faut-il un compte
        // capable d'envoyer. On en connecte un pour isoler ce que ce test
        // vérifie — la provenance de l'adresse.
        crate::connectors::set_status(&db, "google", "google", "connected").unwrap();
        let known = serde_json::json!({
            "to": "paul@exemple.fr",
            "subject": "Bonjour",
            "body": "Je passerai à 18 heures."
        });
        assert!(mail_send_preflight(&db, &known, user, user, true, false).is_none());
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

#[cfg(test)]
mod routing_eval {
    use super::eval::{Route, CORPUS, SUITES, VALIDATION};
    use super::*;

    /// Aiguillage tel qu'il est décidé aujourd'hui dans `handle_query_with_context`,
    /// reproduit à l'identique et dans le même ordre.
    fn route_actuelle(text: &str) -> Route {
        if is_device_diagnostic_query(text) {
            return Route::DeviceDiagnostic;
        }
        if resolve_file_search_request(text, &[]).is_some() {
            return match file_search_scope(text) {
                FileSearchScope::Cloud(Some("google")) => Route::FileSearchGoogle,
                FileSearchScope::Cloud(Some("microsoft")) => Route::FileSearchMicrosoft,
                FileSearchScope::Local => Route::FileSearchLocal,
                _ => Route::FileSearch,
            };
        }
        if is_mail_composition_query(text) {
            return Route::MailCompose;
        }
        // Tout le reste part dans la boucle agentique : le modèle choisit seul
        // s'il appelle un outil. Pour la mesure, c'est « Conversation ».
        Route::Conversation
    }

    /// Une portée non demandée n'est pas une erreur de même gravité qu'une
    /// intention manquée : on compte les deux, séparément.
    fn meme_famille(a: Route, b: Route) -> bool {
        let famille = |route: Route| match route {
            Route::FileSearch
            | Route::FileSearchGoogle
            | Route::FileSearchMicrosoft
            | Route::FileSearchLocal => 0,
            Route::MailCompose => 1,
            Route::MailSearch => 5,
            Route::DeviceDiagnostic => 2,
            Route::DocumentCreate => 3,
            Route::Conversation => 4,
        };
        famille(a) == famille(b)
    }

    #[test]
    fn mesure_du_taux_derreur_de_routage() {
        let mut intention_ratee = Vec::new();
        let mut portee_ratee = Vec::new();
        for case in CORPUS {
            let obtenue = route_actuelle(case.text);
            if !meme_famille(obtenue, case.expected) {
                intention_ratee.push((case, obtenue));
            } else if obtenue != case.expected {
                portee_ratee.push((case, obtenue));
            }
        }
        let total = CORPUS.len();
        println!("\n╭─ Routage : {total} demandes du corpus");
        println!(
            "│  intention manquée : {} ({:.0} %)",
            intention_ratee.len(),
            100.0 * intention_ratee.len() as f64 / total as f64
        );
        println!(
            "│  portée manquée    : {} ({:.0} %)",
            portee_ratee.len(),
            100.0 * portee_ratee.len() as f64 / total as f64
        );
        println!(
            "╰─ total erroné      : {} ({:.0} %)",
            intention_ratee.len() + portee_ratee.len(),
            100.0 * (intention_ratee.len() + portee_ratee.len()) as f64 / total as f64
        );
        println!("\n  Intentions manquées :");
        for (case, obtenue) in &intention_ratee {
            println!(
                "   ✗ {:?} au lieu de {:?}  « {} »\n       ({})",
                obtenue, case.expected, case.text, case.note
            );
        }
        println!("\n  Portées manquées :");
        for (case, obtenue) in &portee_ratee {
            println!(
                "   ~ {:?} au lieu de {:?}  « {} »",
                obtenue, case.expected, case.text
            );
        }
        println!();
    }

    /// Mesure la compréhension EN CONTEXTE : le dernier message d'une suite
    /// d'échanges doit poursuivre l'intention en cours, pas en ouvrir une autre.
    #[tokio::test]
    async fn une_reponse_courte_poursuit_lintention_en_cours() {
        let llm: std::sync::Arc<dyn crate::llm::LlmClient> =
            std::sync::Arc::new(crate::llm::ollama::OllamaClient::new(
                "http://127.0.0.1:11434",
                "llama3.1:latest",
                "nomic-embed-text",
                std::sync::Arc::new(crate::security::egress::EgressGuard::new()),
            ));
        if !llm.status().await.chat_model_ready {
            println!("Modèle absent : mesure ignorée.");
            return;
        }
        let mut ratees = Vec::new();
        for suite in SUITES {
            let contexte: Vec<(String, String)> = suite
                .echanges
                .iter()
                .map(|(role, texte)| (role.to_string(), texte.to_string()))
                .collect();
            let comprise = super::intent::classify(
                &llm,
                suite.dernier,
                &contexte,
                None,
                super::intent::Intent {
                    kind: super::intent::Kind::Conversation,
                    scope: super::intent::Scope::Any,
                    subject: None,
                    reply: None,
                    mail_action: None,
                    source: super::intent::Source::Fallback,
                },
            )
            .await;
            let obtenue = match comprise.kind {
                super::intent::Kind::FileSearch => Route::FileSearch,
                super::intent::Kind::MailSearch => Route::MailSearch,
                super::intent::Kind::MailCompose => Route::MailCompose,
                super::intent::Kind::DeviceDiagnostic => Route::DeviceDiagnostic,
                super::intent::Kind::DocumentCreate => Route::DocumentCreate,
                super::intent::Kind::Conversation => Route::Conversation,
            };
            if !meme_famille(obtenue, suite.expected) {
                ratees.push((suite.dernier, obtenue, suite.expected));
            }
        }
        println!("\n╭─ Suites d'échanges : {} cas", SUITES.len());
        println!(
            "╰─ erronés : {} ({:.0} %)",
            ratees.len(),
            100.0 * ratees.len() as f64 / SUITES.len() as f64
        );
        for (texte, obtenue, attendue) in &ratees {
            println!("   ✗ {obtenue:?} au lieu de {attendue:?}  « {texte} »");
        }
        // Seuil strict, et assumé : sans contexte, ce même jeu échoue. Un seuil
        // tolérant aurait laissé passer le comportement qu'on vient de corriger.
        assert!(
            ratees.is_empty(),
            "une réponse courte ne doit pas changer d'intention : {ratees:?}"
        );
    }

    /// Décompose le temps réellement passé sur un message, avec le prompt et
    /// les exemples de calibrage tels qu'ils sont en production.
    #[tokio::test]
    async fn ou_part_le_temps_dune_reponse() {
        let llm: std::sync::Arc<dyn crate::llm::LlmClient> =
            std::sync::Arc::new(crate::llm::ollama::OllamaClient::new(
                "http://127.0.0.1:11434",
                "llama3.1:latest",
                "nomic-embed-text",
                std::sync::Arc::new(crate::security::egress::EgressGuard::new()),
            ));
        if !llm.status().await.chat_model_ready {
            return;
        }
        let contexte: Vec<(String, String)> = vec![
            (
                "user".into(),
                "Tu pourrais envoyer un mail à paul flaud ?".into(),
            ),
            (
                "assistant".into(),
                "Que voulez-vous dire dans ce mail ?".into(),
            ),
            (
                "user".into(),
                "Dis-lui « Bonjour, ceci est un test »".into(),
            ),
            (
                "assistant".into(),
                "Quel compte d'envoi souhaitez-vous utiliser ?".into(),
            ),
        ];
        let vide = intent::Intent {
            kind: intent::Kind::Conversation,
            scope: intent::Scope::Any,
            subject: None,
            reply: None,
            mail_action: None,
            source: intent::Source::Fallback,
        };
        // Chauffe.
        let _ = intent::classify(&llm, "gmail", &contexte, None, vide.clone()).await;

        let t = std::time::Instant::now();
        let _ = intent::classify(&llm, "gmail", &contexte, None, vide.clone()).await;
        println!("\n  1. compréhension de l'intention : {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let _ = llm.embed(&["gmail".to_string()]).await;
        println!("  2. vectorisation de la demande  : {:?}", t.elapsed());

        let outils = crate::tools::catalog_for(intent::Kind::MailCompose);
        let t = std::time::Instant::now();
        let _ = llm
            .generate(
                "Tu es Syn, un assistant de vie numérique local-first.",
                &[crate::llm::ChatMessage::user("gmail")],
                &outils,
                crate::llm::GenParams {
                    temperature: 0.3,
                    max_tokens: Some(1200),
                    json: false,
                },
            )
            .await;
        println!("  3. une itération agentique      : {:?}", t.elapsed());
        println!(
            "     (la boucle peut en enchaîner jusqu'à {})\n",
            MAX_TOOL_ITERATIONS
        );
    }
    /// Mesure les réponses données EN COURS de parcours — accord, correction,
    /// choix de compte, changement de sujet.
    ///
    /// Deux mesures dans le même passage : le secours à mots-clés d'abord, la
    /// compréhension du modèle ensuite. C'est l'écart entre les deux qui dit si
    /// confier ces décisions au modèle vaut la peine — et le jour où il se
    /// dégrade, on le verra ici plutôt que dans une conversation de Paul.
    #[tokio::test]
    async fn mesure_des_reponses_en_cours_de_parcours() {
        use super::eval::TURNS;
        let mots_cles: Vec<&super::eval::TurnCase> = TURNS
            .iter()
            .filter(|cas| super::mail_flow::reply_fallback(cas.step, cas.text) != cas.expected)
            .collect();
        println!("\n╭─ Réponses en cours de parcours : {} cas", TURNS.len());
        println!(
            "│  secours à mots-clés  : {} erreurs ({:.0} %)",
            mots_cles.len(),
            100.0 * mots_cles.len() as f64 / TURNS.len() as f64
        );

        let llm: std::sync::Arc<dyn crate::llm::LlmClient> =
            std::sync::Arc::new(crate::llm::ollama::OllamaClient::new(
                "http://127.0.0.1:11434",
                "llama3.1:latest",
                "nomic-embed-text",
                std::sync::Arc::new(crate::security::egress::EgressGuard::new()),
            ));
        if !llm.status().await.chat_model_ready {
            println!("╰─ modèle absent : compréhension non mesurée\n");
            return;
        }
        super::intent::preheat(&llm).await;
        let mut ratees = Vec::new();
        for cas in TURNS {
            let comprise = super::intent::classify(
                &llm,
                cas.text,
                &[(
                    "assistant".to_string(),
                    match cas.step {
                        super::intent::Step::DraftReview => {
                            "Voici ce que je te propose d'envoyer. Tu valides ?".to_string()
                        }
                        super::intent::Step::AccountChoice => {
                            "Depuis quel compte souhaites-tu envoyer le mail ?".to_string()
                        }
                        super::intent::Step::SendConfirmation => {
                            "Ce mail est prêt : il attend ta confirmation.".to_string()
                        }
                    },
                )],
                Some(cas.step),
                super::intent::Intent {
                    kind: super::intent::Kind::Conversation,
                    scope: super::intent::Scope::Any,
                    subject: None,
                    reply: None,
                    mail_action: None,
                    source: super::intent::Source::Fallback,
                },
            )
            .await;
            let decision = super::mail_flow::read_reply(
                cas.step,
                comprise.reply,
                cas.text,
                &[
                    ("apple", "Apple Mail"),
                    ("google", "Gmail"),
                    ("microsoft", "Outlook"),
                ],
            );
            if decision != cas.expected {
                ratees.push((cas, Some(decision)));
            }
        }
        println!(
            "╰─ compréhension       : {} erreurs ({:.0} %)",
            ratees.len(),
            100.0 * ratees.len() as f64 / TURNS.len() as f64
        );
        for (cas, obtenue) in &ratees {
            println!(
                "   ✗ {:?} au lieu de {:?}  « {} »  [{}]",
                obtenue, cas.expected, cas.text, cas.note
            );
        }
        for cas in &mots_cles {
            println!(
                "   ~ mots-clés se trompent sur « {} »  [{}]",
                cas.text, cas.note
            );
        }
        println!();
    }

    /// Mesure ce que Syn comprend du GESTE demandé sur des messages : voir sa
    /// boîte, retrouver, lire, jeter. Le même mot « mail » sert aux quatre.
    #[tokio::test]
    async fn mesure_des_gestes_sur_les_messages() {
        use super::eval::MAIL_ACTIONS;
        let secours: Vec<_> = MAIL_ACTIONS
            .iter()
            .filter(|(texte, attendu)| super::mail_action_fallback(texte) != *attendu)
            .collect();
        println!("\n╭─ Gestes sur les messages : {} cas", MAIL_ACTIONS.len());
        println!(
            "│  secours à mots-clés  : {} erreurs ({:.0} %)",
            secours.len(),
            100.0 * secours.len() as f64 / MAIL_ACTIONS.len() as f64
        );
        let llm: std::sync::Arc<dyn crate::llm::LlmClient> =
            std::sync::Arc::new(crate::llm::ollama::OllamaClient::new(
                "http://127.0.0.1:11434",
                "llama3.1:latest",
                "nomic-embed-text",
                std::sync::Arc::new(crate::security::egress::EgressGuard::new()),
            ));
        if !llm.status().await.chat_model_ready {
            println!("╰─ modèle absent : compréhension non mesurée\n");
            return;
        }
        super::intent::preheat(&llm).await;
        let mut ratees = Vec::new();
        for (texte, attendu) in MAIL_ACTIONS {
            let comprise = super::intent::classify(
                &llm,
                texte,
                &[],
                None,
                super::intent::Intent {
                    kind: super::intent::Kind::Conversation,
                    scope: super::intent::Scope::Any,
                    subject: None,
                    reply: None,
                    mail_action: None,
                    source: super::intent::Source::Fallback,
                },
            )
            .await;
            let geste = comprise
                .mail_action
                .unwrap_or_else(|| super::mail_action_fallback(texte));
            if geste != *attendu {
                ratees.push((texte, geste, attendu, comprise.kind));
            }
        }
        println!(
            "╰─ compréhension       : {} erreurs ({:.0} %)",
            ratees.len(),
            100.0 * ratees.len() as f64 / MAIL_ACTIONS.len() as f64
        );
        for (texte, geste, attendu, kind) in &ratees {
            println!("   ✗ {geste:?} au lieu de {attendu:?}  « {texte} »  [intention: {kind:?}]");
        }
        println!();
    }

    /// Mesure la compréhension du modèle local sur le même corpus. Ignoré si
    /// Ollama n'est pas joignable : le secours déterministe reste mesuré par le
    /// test ci-dessus, et le contrat produit suppose le modèle disponible.
    #[tokio::test]
    async fn mesure_du_taux_derreur_avec_comprehension() {
        let llm: std::sync::Arc<dyn crate::llm::LlmClient> =
            std::sync::Arc::new(crate::llm::ollama::OllamaClient::new(
                "http://127.0.0.1:11434",
                "llama3.1:latest",
                "nomic-embed-text",
                std::sync::Arc::new(crate::security::egress::EgressGuard::new()),
            ));
        if !llm.status().await.chat_model_ready {
            println!("Modèle de conversation absent : mesure ignorée.");
            return;
        }

        for (nom, jeu) in [
            ("réglage", CORPUS),
            ("VALIDATION (jamais utilisé pour régler)", VALIDATION),
        ] {
            let mut intention_ratee = Vec::new();
            let mut portee_ratee = Vec::new();
            for case in jeu {
                let comprise = super::intent::classify(
                    &llm,
                    case.text,
                    &[],
                    None,
                    super::intent::Intent {
                        kind: super::intent::Kind::Conversation,
                        scope: super::intent::Scope::Any,
                        subject: None,
                        reply: None,
                        mail_action: None,
                        source: super::intent::Source::Fallback,
                    },
                )
                .await;
                let obtenue = match (comprise.kind, comprise.scope) {
                    (super::intent::Kind::FileSearch, super::intent::Scope::Google) => {
                        Route::FileSearchGoogle
                    }
                    (super::intent::Kind::FileSearch, super::intent::Scope::Microsoft) => {
                        Route::FileSearchMicrosoft
                    }
                    (super::intent::Kind::FileSearch, super::intent::Scope::Local) => {
                        Route::FileSearchLocal
                    }
                    (super::intent::Kind::FileSearch, _) => Route::FileSearch,
                    (super::intent::Kind::MailSearch, _) => Route::MailSearch,
                    (super::intent::Kind::MailCompose, _) => Route::MailCompose,
                    (super::intent::Kind::DeviceDiagnostic, _) => Route::DeviceDiagnostic,
                    (super::intent::Kind::DocumentCreate, _) => Route::DocumentCreate,
                    (super::intent::Kind::Conversation, _) => Route::Conversation,
                };
                if !meme_famille(obtenue, case.expected) {
                    intention_ratee.push((case, obtenue, comprise.subject.clone()));
                } else if obtenue != case.expected {
                    portee_ratee.push((case, obtenue));
                }
            }
            let total = jeu.len();
            let errones = intention_ratee.len() + portee_ratee.len();
            println!("\n╭─ Compréhension · jeu de {nom} : {total} demandes");
            println!(
                "│  intention manquée : {} ({:.0} %)",
                intention_ratee.len(),
                100.0 * intention_ratee.len() as f64 / total as f64
            );
            println!(
                "│  portée manquée    : {} ({:.0} %)",
                portee_ratee.len(),
                100.0 * portee_ratee.len() as f64 / total as f64
            );
            println!(
                "╰─ total erroné      : {errones} ({:.1} %)",
                100.0 * errones as f64 / total as f64
            );
            for (case, obtenue, sujet) in &intention_ratee {
                println!(
                    "   ✗ {:?} au lieu de {:?}  « {} »  [sujet: {:?}]",
                    obtenue, case.expected, case.text, sujet
                );
            }
            for (case, obtenue) in &portee_ratee {
                println!(
                    "   ~ {:?} au lieu de {:?}  « {} »",
                    obtenue, case.expected, case.text
                );
            }
            println!();
        }
    }
}

#[cfg(test)]
mod reponse_tests {
    use super::*;

    /// Cas réel du 17/08/2026 : la réponse affichée contenait le résultat brut
    /// de `people.resolve_email` et l'identifiant interne de l'action.
    #[test]
    fn la_mecanique_interne_natteint_pas_la_conversation() {
        let brut = "Le mail est en attente de confirmation.\n\n\
                    Si vous souhaitez annuler, appelez files.cancel avec l'action_id 1e2db97f-1643-4839-9f75-d92f27d7dc2c.\n\n\
                    {\"matches\":[{\"email\":\"paul@example.com\",\"name\":\"Paul\"}],\"resolved\":true}";
        let propre = strip_internal_noise(brut);
        assert!(!propre.contains("matches"), "{propre}");
        assert!(!propre.contains("1e2db97f"), "{propre}");
        assert!(propre.contains("en attente de confirmation"), "{propre}");
    }

    #[test]
    fn le_resultat_dun_envoi_ne_saffiche_pas() {
        let brut = "Le mail est en attente de confirmation avant d'être envoyé à Paul Flaud.\n\n{\"status\":\"envoyé\",\"subject\":\"Bonjour, ceci est un test\",\"to\":\"paulpro.flaud@gmail.com\",\"via\":\"google\"}";
        let propre = strip_internal_noise(brut);
        println!("APRÈS NETTOYAGE : {propre}");
        assert!(!propre.contains("status"), "{propre}");
    }
    /// Une accolade dans une phrase normale ne doit pas emporter le texte.
    #[test]
    fn une_accolade_ordinaire_est_preservee() {
        let texte = "J'ai trouvé le fichier {brouillon} dans Documents.";
        assert_eq!(strip_internal_noise(texte), texte);
    }
}

#[cfg(test)]
mod memoire_tests {
    use super::*;

    /// Syn ne retient rien de lui-même, mais il ne laisse pas non plus filer ce
    /// qu'il vient d'apprendre : après un envoi vers une adresse inconnue, il
    /// PROPOSE l'association. Le nom vient d'une résolution infructueuse
    /// récente, jamais d'une phrase interprétée.
    #[test]
    fn une_adresse_inconnue_declenche_une_proposition_nommee() {
        let dir = std::env::temp_dir().join(format!("syn-appris-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"9".repeat(64)).unwrap();
        let args = json!({"to": "camille.roux@exemple.fr"});

        // Sans recherche préalable, aucun nom : on ne propose rien plutôt que
        // de fabriquer une fiche à partir d'une adresse.
        assert!(learnable_contact(&db, &args).unwrap().is_none());

        crate::security::log_access(
            &db,
            "people",
            "resolve_email_unresolved",
            Some("Camille Roux"),
        );
        let (nom, adresse) = learnable_contact(&db, &args)
            .unwrap()
            .expect("proposition attendue");
        assert_eq!(nom, "Camille Roux");
        assert_eq!(adresse, "camille.roux@exemple.fr");

        // Une fois la personne connue, plus aucune proposition : pas de doublon.
        crate::memory::find_or_create_person(
            &db,
            "Camille Roux",
            Some("camille.roux@exemple.fr"),
            None,
        )
        .unwrap();
        assert!(learnable_contact(&db, &args).unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// L'enregistrement passe toujours par une confirmation, quel que soit le
    /// niveau d'autonomie — c'est ce qui empêche la mémoire de se remplir toute
    /// seule.
    #[test]
    fn memoriser_un_contact_deduit_se_confirme_a_tout_niveau_dautonomie() {
        for autonomie in [
            crate::settings::Autonomy::Prudent,
            crate::settings::Autonomy::Assiste,
            crate::settings::Autonomy::Autonome,
        ] {
            assert!(
                actions::needs_confirmation(
                    actions::RiskClass::ReversibleLocal,
                    &autonomie,
                    false,
                    "people.link_email"
                ),
                "confirmation attendue pour {autonomie:?}"
            );
        }
    }
}

#[cfg(test)]
mod composition_tests {
    use super::*;
    use crate::connectors::mail;

    /// Rejoue la conversation du 17/08 à 23h36, tour par tour.
    ///
    /// L'utilisateur donne le contenu au tour 2, le compte au tour 3, puis
    /// demande l'envoi. Syn redemandait le contenu et repartait sur une
    /// recherche de documents. L'état d'envoi doit rendre chaque information
    /// définitivement acquise.
    #[test]
    fn un_envoi_se_construit_sur_plusieurs_tours_sans_rien_redemander() {
        let dir = std::env::temp_dir().join(format!("syn-compo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"4".repeat(64)).unwrap();
        crate::connectors::set_status(&db, "google", "google", "connected").unwrap();
        let session = "s-compo";
        let historique = "Tu pourrais envoyer un mail à paulpro.flaud@gmail.com ?";

        // Tour 1 — le destinataire seul. Il manque le contenu.
        let etat =
            mail::remember_composition(&db, session, &json!({"to": "paulpro.flaud@gmail.com"}))
                .unwrap();
        assert_eq!(etat.missing(), vec!["contenu"]);

        // Tour 2 — le contenu. Le destinataire ne doit pas disparaître.
        let etat = mail::remember_composition(
            &db,
            session,
            &json!({"subject": "Test", "body": "Bonjour, ceci est un test"}),
        )
        .unwrap();
        assert_eq!(etat.recipient, "paulpro.flaud@gmail.com");
        assert!(etat.missing().is_empty(), "{etat:?}");

        // Tour 3 — le compte, et rien d'autre.
        let etat = mail::remember_composition(&db, session, &json!({"via": "google"})).unwrap();
        assert_eq!(etat.body, "Bonjour, ceci est un test");
        assert_eq!(etat.via, "google");

        // Tour 4 — le modèle rappelle mail.send SANS aucun argument, comme il
        // le fait quand il a « oublié ». L'état complète, et plus rien ne
        // bloque : la carte de confirmation peut être préparée.
        let complete = json!({
            "to": etat.recipient, "subject": etat.subject,
            "body": etat.body, "via": etat.via
        });
        assert!(
            mail_send_preflight(&db, &complete, "envoie", historique, true, false).is_none(),
            "l'envoi ne devrait plus rien réclamer"
        );

        // Après l'envoi, l'état disparaît : la demande suivante repart à neuf.
        mail::clear_composition(&db, session).unwrap();
        assert!(mail::composition(&db, session).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Le secours hors ligne doit distinguer « retrouve un mail » de « envoie
    /// un mail » : ce sont deux parcours opposés qui partagent leurs mots.
    #[test]
    fn chercher_un_mail_nest_pas_en_ecrire_un() {
        assert!(is_mail_search_query(
            "Tu peux me retrouver un mail de Liverpool qui concerne ma réservation ?"
        ));
        assert!(is_mail_search_query("où est le message du syndic ?"));
        assert!(is_mail_search_query(
            "j'ai reçu un courriel d'Orange, tu le retrouves ?"
        ));
        // Composer, ce n'est pas chercher.
        assert!(!is_mail_search_query("Tu peux envoyer un mail à Paul ?"));
        assert!(!is_mail_search_query(
            "écris un mail à Camille pour la remercier"
        ));
        // Une recherche de document reste une recherche de document.
        assert!(!is_mail_search_query("retrouve mon contrat de location"));
    }

    /// Le parcours d'envoi ne doit rien devoir au mail de test qui a servi à le
    /// corriger. Des tournures, des destinataires et des sujets sans rapport
    /// entre eux — c'est la garde contre une correction taillée pour un cas.
    #[test]
    fn le_parcours_ne_depend_pas_dune_tournure_particuliere() {
        let neutre = intent::Intent {
            kind: intent::Kind::MailCompose,
            scope: intent::Scope::Any,
            subject: None,
            reply: None,
            mail_action: None,
            source: intent::Source::Understood,
        };
        for demande in [
            "Envoie un mail à Camille pour la remercier de son aide",
            "Écris un mail à mon propriétaire pour signaler la fuite d'eau",
            "Tu peux envoyer un courriel à Julie pour lui dire que je serai en retard ?",
            "rédige un email à l'agence pour demander un rendez-vous la semaine prochaine",
            "envoie un mail au syndic : la minuterie du hall ne fonctionne plus",
        ] {
            assert!(
                mail_content_expressed(demande, &neutre),
                "contenu non reconnu : {demande}"
            );
        }

        for accord in [
            "Oui",
            "ok",
            "c'est parfait",
            "ça me va",
            "Je valide",
            "vas-y",
            "👍",
            "c'est très bien",
            "Envoie",
        ] {
            assert!(
                mail_flow::is_draft_approval(accord),
                "accord non reconnu : {accord}"
            );
        }

        for retouche in [
            "non",
            "attends",
            "plutôt plus court",
            "change la première phrase",
            "n'envoie pas ça",
            "ajoute une phrase sur la date",
        ] {
            assert!(
                !mail_flow::is_draft_approval(retouche),
                "pris à tort pour un accord : {retouche}"
            );
        }
    }

    /// Une confirmation d'adresse n'est pas un contenu de mail : la prendre
    /// pour tel faisait rédiger un message sur « c'est correct ».
    #[test]
    fn une_confirmation_dadresse_ne_vaut_pas_contenu() {
        let sans_sujet = |kind| intent::Intent {
            kind,
            scope: intent::Scope::Any,
            subject: None,
            reply: None,
            mail_action: None,
            source: intent::Source::Understood,
        };
        let avec_sujet = |sujet: &str| intent::Intent {
            kind: intent::Kind::MailCompose,
            scope: intent::Scope::Any,
            subject: Some(sujet.to_string()),
            reply: None,
            mail_action: None,
            source: intent::Source::Understood,
        };

        assert!(!mail_content_expressed(
            "C'est correct",
            &sans_sujet(intent::Kind::MailCompose)
        ));
        assert!(!mail_content_expressed(
            "Oui, c'est bien celle-là",
            &sans_sujet(intent::Kind::MailCompose)
        ));
        // Le contenu porté par la demande elle-même.
        assert!(mail_content_expressed(
            "À toi de rédiger un mail pour lui demander s'il est d'accord pour la colocation",
            &sans_sujet(intent::Kind::MailCompose)
        ));
        assert!(mail_content_expressed(
            "dis-lui que je serai en retard",
            &sans_sujet(intent::Kind::MailCompose)
        ));
        // Le sujet compris par le classifieur suffit, sans mot-clé.
        assert!(mail_content_expressed(
            "souhaite-lui un bon anniversaire",
            &avec_sujet("un bon anniversaire")
        ));
    }

    /// Cas réel du 18/08, 14h31 : Syn a écrit « Le mail a été envoyé à
    /// paulpro.flaud@gmail.com » alors qu'aucun outil n'avait tourné. Une
    /// affirmation d'envoi se confronte au journal des actions, jamais à la
    /// bonne foi du modèle.
    #[test]
    fn une_affirmation_denvoi_est_reconnue() {
        assert!(claims_a_sent_mail(
            "Le mail a été envoyé à paulpro.flaud@gmail.com. L'utilisateur a confirmé."
        ));
        assert!(claims_a_sent_mail("C'est envoyé !"));
        assert!(claims_a_sent_mail("J'ai bien envoyé ton message."));
        // Ce que Syn dit quand il n'a rien envoyé ne doit pas se faire prendre
        // pour un aveu d'envoi.
        assert!(!claims_a_sent_mail(
            "Je n'ai encore rien envoyé : ce mail attend ta validation."
        ));
        assert!(!claims_a_sent_mail(
            "Depuis quel compte souhaites-tu envoyer le mail à Paul ?"
        ));
    }

    /// Le blocage vu le 18/08 en conditions réelles : Syn trouve l'adresse de
    /// Paul dans le carnet, l'utilisateur répond « c'est bien celle-là »… et le
    /// contrôle anti-adresse-inventée refusait l'envoi, parce que l'utilisateur
    /// n'avait jamais TAPÉ l'adresse lui-même. Le parcours rendait alors la
    /// main au modèle, qui improvisait.
    #[test]
    fn une_adresse_sortie_du_carnet_est_legitime_sans_etre_tapee() {
        let dir = std::env::temp_dir().join(format!("syn-carnet-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"7".repeat(64)).unwrap();
        crate::connectors::set_status(&db, "google", "google", "connected").unwrap();
        let historique = "Tu peux envoyer un mail à Paul ?\nC'est bien celle-là";
        let args = json!({
            "to": "paulpro.flaud@gmail.com", "subject": "Colocation",
            "body": "Salut Paul, est-ce que tu serais partant ?", "via": "google"
        });

        // Adresse écrite par le modèle : le contrôle tient, comme avant.
        assert_eq!(
            mail_send_preflight(&db, &args, "", historique, true, false).unwrap()["status"],
            "destinataire_non_resolu"
        );
        // Adresse sortie du carnet de l'utilisateur : le parcours passe.
        assert!(
            mail_send_preflight(&db, &args, "", historique, true, true).is_none(),
            "une adresse résolue par Syn doit être acceptée"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// La confiance suit l'adresse, pas la session : dès qu'un appel d'outil en
    /// écrit une autre, on repasse sous contrôle.
    #[test]
    fn la_provenance_du_destinataire_suit_sa_source() {
        let dir = std::env::temp_dir().join(format!("syn-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"8".repeat(64)).unwrap();
        let session = "s-source";

        let resolu =
            mail::remember_resolved_recipient(&db, session, "paulpro.flaud@gmail.com").unwrap();
        assert!(resolu.recipient_is_resolved());

        // Le corps arrive ensuite : la provenance de l'adresse ne bouge pas.
        let redige =
            mail::remember_composition(&db, session, &json!({"body": "Salut Paul"})).unwrap();
        assert!(redige.recipient_is_resolved());

        // Une autre adresse, écrite par le modèle : sous contrôle.
        let reecrit =
            mail::remember_composition(&db, session, &json!({"to": "inconnu@ailleurs.fr"}))
                .unwrap();
        assert!(!reecrit.recipient_is_resolved());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Le parcours des maquettes : Syn rédige, l'utilisateur relit, PUIS on
    /// parle du compte d'envoi. Un texte proposé n'est jamais réputé accepté.
    #[test]
    fn le_texte_redige_par_syn_attend_sa_relecture() {
        let dir = std::env::temp_dir().join(format!("syn-relecture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"6".repeat(64)).unwrap();
        let session = "s-relecture";

        let etat = mail::remember_composition(
            &db,
            session,
            &json!({"to": "paulpro.flaud@gmail.com", "subject": "Joyeux anniversaire",
                    "body": "Hello Paul,\n\nPetit message pour te souhaiter…"}),
        )
        .unwrap();
        assert!(etat.awaits_approval(), "le texte proposé doit être relu");

        // Un rappel d'outil qui ne change pas le corps ne repose pas la question.
        let inchange = mail::remember_composition(&db, session, &json!({"via": "google"})).unwrap();
        assert!(inchange.awaits_approval());

        let approuve = mail::approve_body(&db, session).unwrap();
        assert!(!approuve.awaits_approval());
        assert_eq!(approuve.via, "google");

        // Une réécriture demandée après coup redevient un texte à relire.
        let reecrit = mail::remember_composition(
            &db,
            session,
            &json!({"body": "Hello Paul, joyeux anniversaire !"}),
        )
        .unwrap();
        assert!(reecrit.awaits_approval());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Cas des maquettes : « un mail très concis pour lui souhaiter un bon
    /// anniversaire » ne contient pas le texte du mail, mais dit assez pour que
    /// Syn rédige. Redemander « que veux-tu dire ? » était une boucle.
    #[test]
    fn une_intention_de_message_vaut_matiere_a_rediger() {
        assert!(mail_request_missing_content(
            "Tu pourrais envoyer un mail à Paul ?"
        ));
        assert!(!mail_request_missing_content(
            "Je veux lui envoyer un mail très concis pour lui souhaiter un bon anniversaire"
        ));
        assert!(!mail_request_missing_content(
            "Envoie un mail à Camille pour la remercier de son aide"
        ));
        // Une circonstance n'est pas une intention de message.
        assert!(mail_request_missing_content(
            "Envoie un mail à Paul pour moi"
        ));
    }

    /// Un champ vide ne doit jamais effacer un champ connu — c'est ce qui
    /// faisait perdre le contenu quand le modèle rappelait l'outil.
    #[test]
    fn un_argument_vide_neffce_jamais_une_information_acquise() {
        let dir = std::env::temp_dir().join(format!("syn-compo2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db"), &"5".repeat(64)).unwrap();
        mail::remember_composition(
            &db,
            "s",
            &json!({"to": "a@b.fr", "subject": "Objet", "body": "Corps"}),
        )
        .unwrap();
        let apres = mail::remember_composition(
            &db,
            "s",
            &json!({"to": "", "subject": "", "body": "", "via": "microsoft"}),
        )
        .unwrap();
        assert_eq!(apres.recipient, "a@b.fr");
        assert_eq!(apres.body, "Corps");
        assert_eq!(apres.via, "microsoft");
        let _ = std::fs::remove_dir_all(dir);
    }
}
