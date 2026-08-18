//! Le parcours d'envoi d'un mail, tenu HORS du modèle.
//!
//! Les maquettes décrivent une suite d'étapes courtes : destinataire confirmé →
//! contenu → relecture du texte proposé → choix du compte → confirmation. Rien
//! de tout cela ne peut dépendre de la bonne volonté du modèle : il oubliait le
//! contenu déjà donné, redemandait le compte, ou annonçait un envoi qu'il
//! n'avait pas préparé. L'état vit dans `mail_compositions` (arguments d'outil
//! structurés, jamais du langage interprété) et l'enchaînement vit ici.
//!
//! Ce module ne fait jamais partir un mail : il prépare, il demande, il attend.
//! L'envoi reste derrière la porte d'action (plancher humain, Sécurité §3.2).

use super::{emit_progress, intent, mail_send_preflight, Answer, PendingRef};
use crate::actions;
use crate::bus::BusEvent;
use crate::connectors::mail;
use crate::error::Result;
use crate::llm::{ChatMessage, GenParams};
use crate::memory;
use crate::settings::Settings;
use crate::state::Core;
use serde::Serialize;
use serde_json::{json, Value};

/// Un compte d'envoi proposé à l'utilisateur, avec de quoi l'afficher.
#[derive(Debug, Clone, Serialize)]
pub struct AccountChoice {
    pub via: String,
    pub label: String,
    pub icon: String,
}

fn icon_for(via: &str) -> &'static str {
    match via {
        "google" => "gmail",
        "microsoft" => "outlook",
        _ => "apple-mail",
    }
}

/// Le corps du mail, cité ligne à ligne : l'interface le rend en bloc détaché,
/// comme dans les maquettes, et la mise en forme survit à la persistance du
/// tour (un champ structuré, lui, aurait disparu au rechargement du fil).
fn quote_block(body: &str) -> String {
    body.lines()
        .map(|line| format!("> {line}").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rédige le mail, et le fait entrer dans le parcours.
///
/// Le modèle local n'appelle pas `mail.send` de façon fiable : il écrivait le
/// mail DANS sa réponse (« Voici le contenu du mail : Objet… »), puis, à
/// « c'est très bien », affirmait l'avoir envoyé alors qu'aucun outil n'avait
/// tourné. On ne corrige pas ça par une consigne. Syn demande donc lui-même le
/// texte, avec un contrat étroit — un objet, un corps, rien d'autre — et c'est
/// LUI qui range le résultat dans l'état d'envoi.
///
/// Le texte revient toujours en `draft` : l'utilisateur le relit avant tout.
pub async fn compose(
    core: &Core,
    session_id: &str,
    user_request: &str,
    trusted_user_history: &str,
    settings: &Settings,
) -> Result<Option<Answer>> {
    let composition = mail::composition(&core.db, session_id)?;
    if composition.recipient.is_empty() {
        return Ok(None);
    }
    // Un texte déjà validé ne se réécrit pas dans le dos de l'utilisateur ;
    // un texte en attente de relecture, si — c'est une correction.
    if !composition.body.is_empty() && !composition.awaits_approval() {
        return Ok(None);
    }
    let previous = (!composition.body.is_empty()).then(|| composition.body.clone());
    emit_progress(
        core,
        session_id,
        "compose",
        "Rédaction du message",
        None,
        3,
        5,
        "running",
    );
    let system = format!(
        "Tu rédiges un mail pour l'utilisateur, en français, en son nom. \
         Destinataire : {}. \
         Réponds UNIQUEMENT par un objet JSON {{\"objet\": \"…\", \"corps\": \"…\"}} : \
         aucun commentaire, aucune explication, aucun texte autour. \
         L'objet est court. Le corps est un vrai message : salutation, une à trois phrases, formule de fin. \
         N'invente aucun fait que l'utilisateur n'a pas donné, ne signe pas d'un nom que tu ne connais pas, \
         et n'écris jamais de balise ni de champ « À : » ou « Objet : » dans le corps.",
        composition.recipient
    );
    let demande = match &previous {
        Some(previous) => format!(
            "Texte précédemment proposé :\n{previous}\n\nCorrection demandée par l'utilisateur :\n{user_request}\n\nRéécris le message en tenant compte de cette correction."
        ),
        None => format!(
            "Ce que l'utilisateur veut dire :\n{user_request}\n\nÉchanges précédents :\n{trusted_user_history}"
        ),
    };
    let response = core
        .llm
        .generate(
            &system,
            &[ChatMessage::user(&demande)],
            &[],
            GenParams {
                temperature: 0.4,
                max_tokens: Some(600),
                json: true,
            },
        )
        .await?;
    let Some((subject, body)) = parse_draft(&response.content) else {
        // Rédaction inexploitable : on le dit, plutôt que de laisser le modèle
        // improviser une réponse qui ressemblerait à un envoi.
        let text = settings
            .voice
            .pick(
                "Je n'ai pas réussi à rédiger ce message. Redis-moi en une phrase ce qu'il doit dire.",
                "Je n'ai pas réussi à rédiger ce message. Redites-moi en une phrase ce qu'il doit dire.",
            )
            .to_string();
        memory::persist_turn(&core.db, session_id, "assistant", &text)?;
        return Ok(Some(Answer {
            text,
            sources: vec![],
            pending_actions: vec![],
            choices: vec![],
            session_id: session_id.into(),
            degraded: true,
        }));
    };
    mail::remember_composition(
        &core.db,
        session_id,
        &json!({ "subject": subject, "body": body }),
    )?;
    advance(core, session_id, settings, trusted_user_history)
}

/// Extrait l'objet et le corps de la réponse du modèle. Un JSON attendu, mais
/// jamais présumé : un modèle local rend parfois du texte autour.
fn parse_draft(content: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(content.trim()).ok().or_else(|| {
        let start = content.find('{')?;
        let end = content.rfind('}')?;
        serde_json::from_str(content.get(start..=end)?).ok()
    })?;
    let subject = value["objet"]
        .as_str()
        .or_else(|| value["subject"].as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let body = value["corps"]
        .as_str()
        .or_else(|| value["body"].as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    (!body.is_empty()).then_some((
        if subject.is_empty() {
            "(sans objet)".to_string()
        } else {
            subject
        },
        body,
    ))
}

/// Où en est l'envoi, et donc ce que Syn doit demander maintenant.
///
/// Rend `None` quand rien de déterministe n'est possible : la main reste alors
/// au modèle (destinataire non résolu, aucun compte configuré, contenu absent).
pub fn advance(
    core: &Core,
    session_id: &str,
    settings: &Settings,
    trusted_user_history: &str,
) -> Result<Option<Answer>> {
    let db = &core.db;
    let composition = mail::composition(db, session_id)?;
    if !composition.missing().is_empty() {
        return Ok(None);
    }
    let args = json!({
        "to": composition.recipient,
        "subject": if composition.subject.is_empty() { "(sans objet)" } else { &composition.subject },
        "body": composition.body,
        "via": composition.via,
    });
    // La légitimité du destinataire reste vérifiée avant toute étape de confort :
    // seule l'absence de compte choisi est une question qu'on sait poser ici.
    match mail_send_preflight(
        db,
        &args,
        "",
        trusted_user_history,
        true,
        composition.recipient_is_resolved(),
    ) {
        None => {}
        Some(reason) if reason["status"] == "compte_a_choisir" => {}
        Some(_) => return Ok(None),
    }

    // 1. Le texte écrit par Syn se relit avant tout le reste.
    if composition.awaits_approval() {
        return Ok(Some(ask_review(core, session_id, &composition, settings)?));
    }

    // 2. Le compte d'envoi appartient à l'utilisateur, pas à un défaut caché.
    let channels = mail::available_channels(db);
    let via = if composition.via.is_empty() {
        match channels.as_slice() {
            [] => return Ok(None),
            [(only, _)] => {
                mail::remember_composition(db, session_id, &json!({ "via": only }))?;
                (*only).to_string()
            }
            _ => {
                return Ok(Some(ask_account(
                    core,
                    session_id,
                    &composition.recipient,
                    &channels,
                    settings,
                )?))
            }
        }
    } else {
        composition.via.clone()
    };

    // 3. Tout est connu : la carte de confirmation, et rien de plus.
    Ok(Some(queue_confirmation(
        core,
        session_id,
        &composition,
        &via,
        settings,
    )?))
}

/// Réponse de l'utilisateur à une étape en cours (relecture, choix du compte
/// tapé au clavier plutôt que cliqué). Traité avant le modèle : « oui » ne doit
/// pas coûter un aller-retour d'inférence, ni risquer une reformulation.
/// L'étape que Syn attend de l'utilisateur, s'il en attend une.
///
/// C'est elle qui donne son sens à « oui » : la même réponse vaut validation
/// d'un texte ou choix d'un compte selon le moment. Elle est calculée sur
/// l'ÉTAT d'envoi, jamais sur les mots du dernier message.
pub fn pending_step(db: &crate::db::Db, session_id: &str) -> Result<Option<intent::Step>> {
    // Un envoi déjà préparé attend un accord : c'est l'étape la plus
    // conséquente, et c'était la seule encore décidée par une liste de mots —
    // une liste qui pouvait faire PARTIR un mail.
    let envoi_en_attente = actions::list_pending(db)?.into_iter().any(|action| {
        action.tool == "mail.send" && action.session_id.as_deref() == Some(session_id)
    });
    if envoi_en_attente {
        return Ok(Some(intent::Step::SendConfirmation));
    }
    let composition = mail::composition(db, session_id)?;
    if composition.recipient.is_empty() || composition.body.is_empty() {
        return Ok(None);
    }
    if composition.awaits_approval() {
        return Ok(Some(intent::Step::DraftReview));
    }
    if composition.via.is_empty() && mail::available_channels(db).len() > 1 {
        return Ok(Some(intent::Step::AccountChoice));
    }
    Ok(None)
}

/// La décision effective sur une réponse en cours de parcours.
///
/// Ce que le modèle a compris fait foi. Il est complété sur un seul point, et
/// seulement à l'étape du compte : la reconnaissance des NOMS PROPRES des
/// messageries configurées. Nommer une de ses trois boîtes au moment précis où
/// Syn demande laquelle n'est pas une devinette de langage — c'est un
/// inventaire fermé, comme reconnaître « OneDrive » dans une portée.
pub fn read_reply(
    step: intent::Step,
    understood: Option<intent::Reply>,
    text: &str,
    channels: &[(&'static str, &'static str)],
) -> intent::Reply {
    if step == intent::Step::AccountChoice {
        if let Some(intent::Reply::Compte(via)) = understood {
            return intent::Reply::Compte(via);
        }
        if let Some(via) = match_typed_channel(text, channels) {
            return intent::Reply::Compte(via);
        }
    }
    understood.unwrap_or_else(|| reply_fallback(step, text))
}

/// Lecture de secours d'une réponse en cours de parcours.
///
/// Elle ne s'applique QUE si le modèle n'a rien pu dire — arrêté, ou au-delà de
/// son budget. C'est une dégradation annoncée : elle ne reconnaît que les
/// formes les plus explicites, et se trompe sur tout le reste. Ce n'est plus
/// elle qui décide en fonctionnement normal.
pub fn reply_fallback(step: intent::Step, text: &str) -> intent::Reply {
    match step {
        intent::Step::SendConfirmation => {
            if super::is_explicit_chat_confirmation(text) {
                intent::Reply::Accord
            } else {
                intent::Reply::Autre
            }
        }
        intent::Step::DraftReview => {
            if is_draft_approval(text) {
                intent::Reply::Accord
            } else {
                intent::Reply::Correction
            }
        }
        intent::Step::AccountChoice => {
            match match_typed_channel(text, &[("apple", ""), ("google", ""), ("microsoft", "")]) {
                Some(via) => intent::Reply::Compte(via),
                None => intent::Reply::Autre,
            }
        }
    }
}

/// Fait avancer le parcours avec ce que l'utilisateur vient de répondre.
///
/// Le sens de sa réponse est COMPRIS (par le modèle, ou par le secours) ; ce
/// qui est fait de ce sens est déterministe. Une correction rend la main au
/// modèle pour réécrire ; un accord ou un compte font avancer l'état.
pub fn handle_user_reply(
    core: &Core,
    session_id: &str,
    step: intent::Step,
    reply: intent::Reply,
    settings: &Settings,
) -> Result<Option<Answer>> {
    let db = &core.db;
    let composition = mail::composition(db, session_id)?;
    if composition.recipient.is_empty() || composition.body.is_empty() {
        return Ok(None);
    }
    match (step, reply) {
        // Le texte proposé est accepté : l'étape suivante s'enchaîne.
        (intent::Step::DraftReview, intent::Reply::Accord) => {
            mail::approve_body(db, session_id)?;
        }
        // Un compte désigné, à l'étape où on l'attend.
        (intent::Step::AccountChoice, intent::Reply::Compte(via)) => {
            if !mail::available_channels(db)
                .iter()
                .any(|(id, _)| *id == via)
            {
                return Ok(None);
            }
            mail::remember_composition(db, session_id, &json!({ "via": via }))?;
        }
        // Une correction est une demande de réécriture : elle appartient au
        // modèle, pas au parcours.
        (_, intent::Reply::Correction) => return Ok(None),
        // Tout le reste — une autre demande, un compte cité hors de son étape —
        // ne fait pas avancer l'envoi.
        _ => return Ok(None),
    }

    let history = memory::recent_turns(db, session_id, 12)?
        .iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    match advance(core, session_id, settings, &history)? {
        Some(answer) => Ok(Some(answer)),
        None => Ok(Some(explain_blockage(
            core,
            session_id,
            &composition,
            &history,
            settings,
        )?)),
    }
}

/// Ce qui empêche l'envoi, dit en une phrase.
///
/// Sans cela, un envoi bloqué rendait la main au modèle, qui repartait sur
/// autre chose : l'utilisateur voyait une réponse hors sujet au lieu d'un
/// obstacle nommé.
fn explain_blockage(
    core: &Core,
    session_id: &str,
    composition: &mail::Composition,
    trusted_user_history: &str,
    settings: &Settings,
) -> Result<Answer> {
    let args = json!({
        "to": composition.recipient,
        "subject": if composition.subject.is_empty() { "(sans objet)" } else { &composition.subject },
        "body": composition.body,
        "via": composition.via,
    });
    let reason = mail_send_preflight(
        &core.db,
        &args,
        "",
        trusted_user_history,
        true,
        composition.recipient_is_resolved(),
    );
    let voice = &settings.voice;
    let text = match reason
        .as_ref()
        .and_then(|reason| reason["status"].as_str())
        .unwrap_or_default()
    {
        "aucun_compte_denvoi" => voice.pick(
            "Aucun compte ne peut envoyer de mail depuis cette machine. Connecte Google ou Microsoft dans Connecteurs, ou demande-moi d'enregistrer un brouillon.",
            "Aucun compte ne peut envoyer de mail depuis cette machine. Connectez Google ou Microsoft dans Connecteurs, ou demandez-moi d'enregistrer un brouillon.",
        ),
        "destinataire_non_resolu" => voice.pick(
            "Je n'ai pas d'adresse sûre pour ce destinataire. Donne-la-moi et je prépare l'envoi.",
            "Je n'ai pas d'adresse sûre pour ce destinataire. Donnez-la-moi et je prépare l'envoi.",
        ),
        "compte_indisponible" => voice.pick(
            "Ce compte d'envoi n'est plus disponible. Dis-moi lequel utiliser à la place.",
            "Ce compte d'envoi n'est plus disponible. Dites-moi lequel utiliser à la place.",
        ),
        "incomplet" => voice.pick(
            "Il me manque encore le texte du message.",
            "Il me manque encore le texte du message.",
        ),
        _ => voice.pick(
            "Je ne peux pas préparer cet envoi pour l'instant.",
            "Je ne peux pas préparer cet envoi pour l'instant.",
        ),
    }
    .to_string();
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "clarify",
        "Envoi impossible en l'état",
        None,
        5,
        5,
        "waiting",
    );
    Ok(Answer {
        text,
        sources: vec![],
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Le compte choisi par un clic sur l'une des propositions.
pub fn choose_account(
    core: &Core,
    session_id: &str,
    via: &str,
    settings: &Settings,
) -> Result<Answer> {
    let db = &core.db;
    let channels = mail::available_channels(db);
    let (via, label) = channels
        .iter()
        .find(|(id, _)| *id == via)
        .copied()
        .ok_or_else(|| {
            crate::error::AppError::Invalid(format!(
                "Ce compte d'envoi n'est pas disponible : {via}."
            ))
        })?;
    mail::remember_composition(db, session_id, &json!({ "via": via }))?;
    // Trace discrète du choix, comme dans les maquettes : ce n'est pas une
    // parole de l'utilisateur, donc ce n'est ni une bulle ni un tour donné au
    // modèle — juste un accusé lisible dans le fil.
    note(
        core,
        session_id,
        &format!(
            "{} « {label} »",
            settings.voice.pick("Tu as choisi", "Vous avez choisi")
        ),
    )?;
    let history = memory::recent_turns(db, session_id, 12)?
        .iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    match advance(core, session_id, settings, &history)? {
        Some(answer) => Ok(answer),
        // Le compte est enregistré mais quelque chose d'autre manque : on le dit
        // plutôt que de laisser l'interface devant une réponse vide.
        None => {
            let text = "C'est noté. Il me manque encore quelque chose pour préparer cet envoi."
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
    }
}

/// Étape « relecture » : Syn a rédigé, l'utilisateur lit avant tout engagement.
fn ask_review(
    core: &Core,
    session_id: &str,
    composition: &mail::Composition,
    settings: &Settings,
) -> Result<Answer> {
    let voice = &settings.voice;
    let text = format!(
        "{} {} :\n\n{}\n\n{}",
        voice.pick(
            "Voici ce que je te propose d'envoyer à",
            "Voici ce que je vous propose d'envoyer à"
        ),
        composition.recipient,
        quote_block(&composition.body),
        voice.pick("Tu valides ?", "Vous validez ?"),
    );
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "clarify",
        "Relecture du message",
        None,
        5,
        5,
        "waiting",
    );
    Ok(Answer {
        text,
        sources: vec![],
        pending_actions: vec![],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Étape « compte » : le choix s'affiche, il ne se devine pas.
fn ask_account(
    core: &Core,
    session_id: &str,
    recipient: &str,
    channels: &[(&'static str, &'static str)],
    settings: &Settings,
) -> Result<Answer> {
    let text = format!(
        "{} {recipient} ?",
        settings.voice.pick(
            "Depuis quel compte souhaites-tu envoyer le mail à",
            "Depuis quel compte souhaitez-vous envoyer le mail à",
        )
    );
    memory::persist_turn(&core.db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "clarify",
        "Choix du compte d'envoi",
        None,
        5,
        5,
        "waiting",
    );
    Ok(Answer {
        text,
        sources: vec![],
        pending_actions: vec![],
        choices: channels
            .iter()
            .map(|(via, label)| AccountChoice {
                via: (*via).to_string(),
                label: (*label).to_string(),
                icon: icon_for(via).to_string(),
            })
            .collect(),
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Dernière étape : la carte de confirmation. Le mail est prêt, rien n'est parti.
fn queue_confirmation(
    core: &Core,
    session_id: &str,
    composition: &mail::Composition,
    via: &str,
    settings: &Settings,
) -> Result<Answer> {
    let db = &core.db;
    // Déjà en attente : une deuxième carte pour le même envoi ferait croire à
    // deux mails.
    if let Some(existing) = actions::list_pending(db)?.into_iter().find(|action| {
        action.tool == "mail.send" && action.session_id.as_deref() == Some(session_id)
    }) {
        let text = settings
            .voice
            .pick(
                "Ce mail est prêt : il attend ta confirmation juste en dessous.",
                "Ce mail est prêt : il attend votre confirmation juste en dessous.",
            )
            .to_string();
        memory::persist_turn(db, session_id, "assistant", &text)?;
        return Ok(Answer {
            text,
            sources: vec![],
            pending_actions: vec![PendingRef {
                action_id: existing.id,
                tool: "mail.send".into(),
                preview: existing.preview,
                risk_class: existing.risk_class,
            }],
            choices: vec![],
            session_id: session_id.into(),
            degraded: false,
        });
    }
    let mut verified = json!({
        "to": composition.recipient,
        "subject": if composition.subject.is_empty() { "(sans objet)" } else { &composition.subject },
        "body": composition.body,
        "via": via,
    });
    // Marqueur posé seulement après les contrôles de `advance` : une action en
    // attente ne peut pas partir avec une adresse jamais vérifiée.
    verified["_syn_preflight_v1"] = json!(true);
    let risk = actions::classify("mail.send", &verified);
    let preview = crate::tools::preview_for("mail.send", &verified);
    let action_id = actions::queue_pending(
        db,
        "mail.send",
        &verified,
        risk,
        &preview,
        false,
        Some(session_id),
    )?;
    core.bus.emit(BusEvent::ActionAwaitingConfirmation {
        action_id: action_id.clone(),
        tool: "mail.send".into(),
        preview: preview.clone(),
        risk_class: risk.as_str().into(),
    });
    let text = format!(
        "D'accord, j'envoie ce mail à {} {} {} :",
        composition.recipient,
        settings
            .voice
            .pick("depuis ton compte", "depuis votre compte"),
        mail::channel_label(via),
    );
    memory::persist_turn(db, session_id, "assistant", &text)?;
    emit_progress(
        core,
        session_id,
        "confirm",
        "Validation utilisateur requise",
        Some(preview.clone()),
        5,
        5,
        "waiting",
    );
    Ok(Answer {
        text,
        sources: vec![],
        pending_actions: vec![PendingRef {
            action_id,
            tool: "mail.send".into(),
            preview,
            risk_class: risk.as_str().into(),
        }],
        choices: vec![],
        session_id: session_id.into(),
        degraded: false,
    })
}

/// Accusé discret dans le fil (« Vous avez choisi Gmail »). Rôle `note` : ni
/// parole de l'utilisateur, ni parole de Syn — il n'entre donc jamais dans le
/// contexte envoyé au modèle.
pub fn note(core: &Core, session_id: &str, text: &str) -> Result<()> {
    memory::persist_turn(&core.db, session_id, "note", text)
}

/// « Oui, ça me va » vaut accord. « Oui mais plus court » n'en est pas un, et
/// « demande-lui s'il est d'accord » n'en est pas un non plus : un accord est
/// une réponse BRÈVE. Sans ce garde-fou, une consigne de rédaction contenant le
/// mot « accord » se faisait prendre pour une validation.
pub fn is_draft_approval(text: &str) -> bool {
    let folded = crate::db::fold(text);
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if [
        "pas",
        "non",
        "mais",
        "plutot",
        "change",
        "modifie",
        "corrige",
        "reformule",
        "ajoute",
        "enleve",
        "retire",
        "raccourcis",
        "rallonge",
        "attends",
        "annule",
    ]
    .iter()
    .any(|term| words.contains(term))
    {
        return false;
    }
    if words.len() > 8 {
        return false;
    }
    [
        "ca me va",
        "ca marche",
        "c'est bon",
        "cest bon",
        "c'est parfait",
        "c'est correct",
        "c'est bien cela",
        "c'est bien ca",
        "c'est bien celle",
        "c'est tres bien",
        "tres bien",
        "tout a fait",
        "impeccable",
        "nickel",
        "envoie",
        "vas-y",
        "vas y",
        "👍",
    ]
    .iter()
    .any(|term| folded.contains(term))
        || [
            "oui",
            "ok",
            "parfait",
            "valide",
            "correct",
            "exactement",
            "d'accord",
            "daccord",
            "accord",
        ]
        .iter()
        .any(|term| words.contains(term))
}

/// Le compte peut aussi être donné au clavier plutôt que cliqué.
pub fn match_typed_channel(
    text: &str,
    channels: &[(&'static str, &'static str)],
) -> Option<&'static str> {
    let folded = crate::db::fold(text);
    // Deux comptes nommés dans la même phrase : on ne choisit pas à sa place.
    let named: Vec<&'static str> = [
        ("google", ["gmail", "google"].as_slice()),
        ("microsoft", ["outlook", "microsoft", "hotmail"].as_slice()),
        ("apple", ["apple", "mail.app"].as_slice()),
    ]
    .iter()
    .filter(|(via, terms)| {
        channels.iter().any(|(id, _)| id == via) && terms.iter().any(|term| folded.contains(term))
    })
    .map(|(via, _)| *via)
    .collect();
    match named.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approbation_et_demande_de_reecriture() {
        assert!(is_draft_approval("Oui, ça me va 👍"));
        assert!(is_draft_approval("ok parfait"));
        assert!(is_draft_approval("C'est bon pour moi"));
        assert!(!is_draft_approval("Oui mais fais plus court"));
        assert!(!is_draft_approval("change la dernière phrase"));
        assert!(!is_draft_approval("n'envoie pas"));
        // Piège réel : une consigne de rédaction qui contient « d'accord ».
        assert!(!is_draft_approval(
            "À toi de rédiger un mail pour lui demander s'il est d'accord pour la colocation"
        ));
    }

    #[test]
    fn compte_tape_au_clavier() {
        let channels = [("apple", "Apple Mail"), ("google", "Gmail")];
        assert_eq!(match_typed_channel("Gmail", &channels), Some("google"));
        assert_eq!(
            match_typed_channel("depuis Apple Mail stp", &channels),
            Some("apple")
        );
        // Compte non configuré, ou ambigu : la question reste posée.
        assert_eq!(match_typed_channel("Outlook", &channels), None);
        assert_eq!(match_typed_channel("gmail ou apple ?", &channels), None);
    }

    /// Le modèle local rend rarement du JSON pur : il l'enrobe.
    #[test]
    fn la_redaction_est_lue_meme_entouree_de_bavardage() {
        let (objet, corps) =
            parse_draft(r#"{"objet":"Colocation ?","corps":"Bonjour Paul,\n\nÀ bientôt !"}"#)
                .unwrap();
        assert_eq!(objet, "Colocation ?");
        assert_eq!(corps, "Bonjour Paul,\n\nÀ bientôt !");

        let (objet, _) = parse_draft(
            "Voici le mail :\n{\"objet\": \"Bon anniversaire\", \"corps\": \"Hello Paul\"}\nVoilà.",
        )
        .unwrap();
        assert_eq!(objet, "Bon anniversaire");

        // Un objet manquant ne bloque pas ; un corps manquant, si.
        assert_eq!(
            parse_draft(r#"{"corps":"Hello"}"#).unwrap().0,
            "(sans objet)"
        );
        assert!(parse_draft(r#"{"objet":"Vide","corps":""}"#).is_none());
        assert!(parse_draft("désolé, je ne peux pas").is_none());
    }

    #[test]
    fn le_corps_est_cite_ligne_a_ligne() {
        assert_eq!(
            quote_block("Bonjour,\n\nÀ bientôt !"),
            "> Bonjour,\n>\n> À bientôt !"
        );
    }
}
