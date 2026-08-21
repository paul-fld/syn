//! Rangement des boîtes cloud : audit borné → plan figé → confirmation →
//! opérations réversibles. Le fournisseur demandé est inscrit dans le plan et
//! revérifié à l'exécution ; un plan Gmail ne peut donc jamais toucher Outlook.

use crate::db::{now, Db};
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const MAX_ACTIONS_PER_PLAN: usize = 2_000;
const MAX_INSPECTIONS_PER_PLAN: usize = 3_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedMail {
    pub id: String,
    pub title: String,
    pub sender: String,
    pub reason: String,
    pub received_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedUnsubscribe {
    pub sender: String,
    pub url: String,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupPlan {
    #[serde(default)]
    pub version: u8,
    pub provider: String,
    pub scanned: usize,
    pub indexed: usize,
    #[serde(default)]
    pub conversation_count: Option<usize>,
    #[serde(default)]
    pub unread_count: Option<usize>,
    pub archive: Vec<PlannedMail>,
    pub trash: Vec<PlannedMail>,
    pub unsubscribe: Vec<PlannedUnsubscribe>,
    pub kept: usize,
    pub review: usize,
    #[serde(default)]
    pub untouched: usize,
    #[serde(default)]
    pub rule_applied: usize,
    pub deferred: usize,
    pub top_bulk_senders: Vec<(String, usize)>,
    pub created_at: i64,
}

#[derive(Debug)]
struct IndexedMail {
    id: String,
    title: String,
    body: String,
    sender: String,
    received_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Keep,
    Review,
    Archive,
    Trash,
}

fn provider_prefix(provider: &str) -> Result<&'static str> {
    match provider {
        "google" => Ok("google:gmail:"),
        "microsoft" => Ok("microsoft:mail:"),
        _ => Err(AppError::Invalid(
            "Le rangement est disponible uniquement pour Gmail ou Outlook.".into(),
        )),
    }
}

fn sender_from_body(body: &str) -> String {
    body.lines()
        .find_map(|line| line.strip_prefix("De : "))
        .map(str::trim)
        .filter(|sender| !sender.is_empty())
        .unwrap_or("Expéditeur inconnu")
        .chars()
        .take(160)
        .collect()
}

fn contains_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.contains(word))
}

/// Classification conservatrice et explicable. Les signaux importants sont
/// évalués avant les signaux marketing : un reçu contenant un pied de page de
/// désabonnement reste conservé. Tout doute reste dans la boîte.
fn classify_mail(mail: &IndexedMail, current_time: i64) -> (Decision, String, String) {
    let sender = if mail.sender.trim().is_empty() {
        sender_from_body(&mail.body)
    } else {
        mail.sender.clone()
    };
    let sender_folded = crate::db::fold(&sender);
    let text = crate::db::fold(&format!(
        "{} {} {}",
        mail.title,
        sender,
        mail.body.chars().take(5_000).collect::<String>()
    ));
    let age_days = current_time.saturating_sub(mail.received_at) / 86_400;

    let durable_record = contains_any(
        &text,
        &[
            "facture",
            "invoice",
            "receipt",
            "recu de paiement",
            "quittance",
            "contrat",
            "contract",
            "boarding pass",
            "carte d'embarquement",
            "assurance",
            "insurance",
            "impot",
            "tax",
            "salaire",
            "payslip",
            "banque",
            "bank",
            "virement",
            "transfer",
            "confirmation de commande",
            "order confirmation",
            "livraison",
            "tracking",
            "rendez-vous",
            "appointment",
            "medical",
            "sante",
            "security alert",
            "alerte de securite",
            "verification code",
            "code de verification",
            "mot de passe",
            "password",
            "legal",
            "juridique",
        ],
    );
    let booked_event = contains_any(&text, &["billet", "ticket", "reservation", "booking"])
        && contains_any(
            &text,
            &[
                "confirmation",
                "confirmed",
                "transaction",
                "reference",
                "seat",
                "siege",
                "total paid",
                "montant paye",
                "supporter details",
            ],
        );
    if durable_record || booked_event {
        return (
            Decision::Keep,
            "information potentiellement utile".into(),
            sender,
        );
    }

    let mut marketing = 0u8;
    if contains_any(
        &text,
        &[
            "unsubscribe",
            "se desabonner",
            "desabonnement",
            "preferences marketing",
            "email preferences",
            "communication commerciale",
        ],
    ) {
        marketing += 3;
    }
    if contains_any(
        &text,
        &[
            "promotion",
            "promo",
            "offre",
            "offer",
            "soldes",
            "sale",
            "newsletter",
            "nouveaute",
            "new collection",
            "deal",
            "coupon",
            "code promo",
            "black friday",
            "marketing",
        ],
    ) {
        marketing += 2;
    }
    if contains_any(
        &text,
        &["% off", "-20%", "-30%", "-40%", "-50%", "prix exceptionnel"],
    ) {
        marketing += 1;
    }
    if contains_any(
        &sender_folded,
        &["newsletter", "marketing", "campaign", "promo", "offers"],
    ) {
        marketing += 2;
    }
    if marketing >= 3 {
        return if age_days >= 365 {
            (
                Decision::Trash,
                format!("communication marketing ancienne ({age_days} jours)"),
                sender,
            )
        } else {
            (Decision::Archive, "communication marketing".into(), sender)
        };
    }

    let automated = contains_any(
        &sender_folded,
        &[
            "no-reply",
            "noreply",
            "do-not-reply",
            "notification",
            "mailer-daemon",
        ],
    ) || contains_any(
        &text,
        &[
            "notification automatique",
            "automated notification",
            "ne pas repondre",
            "do not reply",
            "weekly digest",
            "daily digest",
            "resume hebdomadaire",
        ],
    );
    if automated && age_days >= 30 {
        return (
            Decision::Archive,
            format!("notification automatique ancienne ({age_days} jours)"),
            sender,
        );
    }

    if sender == "Expéditeur inconnu" || mail.title.trim().is_empty() {
        (
            Decision::Review,
            "informations insuffisantes".into(),
            sender,
        )
    } else {
        (
            Decision::Keep,
            "message personnel ou utilité possible".into(),
            sender,
        )
    }
}

fn topic_matches(topic: &str, text: &str) -> bool {
    match topic {
        "invoice" => contains_any(text, &["facture", "invoice", "receipt", "recu"]),
        "booking" => contains_any(text, &["reservation", "booking", "billet", "ticket"]),
        "marketing" => contains_any(
            text,
            &["promotion", "promo", "newsletter", "offre", "offer", "unsubscribe"],
        ),
        "notification" => contains_any(text, &["notification", "alerte", "digest"]),
        _ => false,
    }
}

fn rule_matches(rule: &crate::rules::MailCleanupRule, mail: &IndexedMail) -> bool {
    let sender = crate::db::fold(&mail.sender);
    let text = crate::db::fold(&format!("{} {}", mail.title, mail.body));
    rule.sender_terms.iter().all(|term| sender.contains(term))
        && rule.topics.iter().all(|topic| topic_matches(topic, &text))
}

fn apply_rule(
    rules: &[crate::rules::MailCleanupRule],
    mail: &IndexedMail,
) -> Option<(Decision, String)> {
    rules.iter().find_map(|rule| {
        if !rule_matches(rule, mail) {
            return None;
        }
        let decision = match rule.action.as_str() {
            "archive" => Decision::Archive,
            "trash" => Decision::Trash,
            "keep" => Decision::Keep,
            _ => return None,
        };
        Some((decision, "règle utilisateur prioritaire".into()))
    })
}

fn cap_actions(archive: &mut Vec<PlannedMail>, trash: &mut Vec<PlannedMail>) -> usize {
    let proposed = archive.len() + trash.len();
    if proposed > MAX_ACTIONS_PER_PLAN {
        let keep_trash = trash.len().min(MAX_ACTIONS_PER_PLAN);
        trash.truncate(keep_trash);
        archive.truncate(MAX_ACTIONS_PER_PLAN - keep_trash);
    }
    proposed.saturating_sub(archive.len() + trash.len())
}

pub async fn build_plan(db: &Db, provider: &str) -> Result<CleanupPlan> {
    provider_prefix(provider)?;
    let rules = crate::rules::active_mail_cleanup_rules(db, provider)?;
    let inventory = crate::connectors::external::mail_cleanup_inventory(
        provider,
        &rules,
        MAX_INSPECTIONS_PER_PLAN,
    )
    .await?;
    let indexed_count = inventory.inspected.len();
    let current_time = now();
    let mut archive = Vec::new();
    let mut trash = Vec::new();
    let mut kept = 0usize;
    let mut review = 0usize;
    let mut rule_applied = 0usize;
    let mut senders: HashMap<String, usize> = HashMap::new();
    let mut sender_samples: HashMap<String, String> = HashMap::new();

    for remote in inventory.inspected {
        let mail = IndexedMail {
            id: remote.id,
            title: remote.title,
            body: remote.body,
            sender: remote.sender,
            received_at: remote.received_at,
        };
        let automatic = classify_mail(&mail, current_time);
        let (decision, reason, sender) = if let Some((decision, reason)) = apply_rule(&rules, &mail) {
            rule_applied += 1;
            (decision, reason, mail.sender.clone())
        } else {
            automatic
        };
        let planned = PlannedMail {
            id: mail.id,
            title: mail.title.chars().take(180).collect(),
            sender: sender.clone(),
            reason,
            received_at: mail.received_at,
        };
        match decision {
            Decision::Archive => {
                *senders.entry(sender).or_default() += 1;
                sender_samples
                    .entry(planned.sender.clone())
                    .or_insert_with(|| planned.id.clone());
                archive.push(planned);
            }
            Decision::Trash => {
                *senders.entry(sender).or_default() += 1;
                sender_samples
                    .entry(planned.sender.clone())
                    .or_insert_with(|| planned.id.clone());
                trash.push(planned);
            }
            Decision::Keep => kept += 1,
            Decision::Review => review += 1,
        }
    }
    archive.sort_by_key(|mail| mail.received_at);
    trash.sort_by_key(|mail| mail.received_at);

    // Un clic ne déclenche jamais une opération sans borne. Les plus anciens
    // passent d'abord ; le reste sera repris par un audit suivant.
    let deferred = cap_actions(&mut archive, &mut trash);

    let mut top_bulk_senders: Vec<(String, usize)> = senders.into_iter().collect();
    top_bulk_senders.sort_by(|left, right| right.1.cmp(&left.1));
    top_bulk_senders.truncate(8);
    let sample_ids = top_bulk_senders
        .iter()
        .filter_map(|(sender, _)| sender_samples.get(sender).cloned())
        .collect::<Vec<_>>();
    let unsubscribe_urls =
        crate::connectors::external::mail_one_click_unsubscribe_options(provider, &sample_ids)
            .await;
    let unsubscribe = top_bulk_senders
        .iter()
        .filter_map(|(sender, count)| {
            let id = sender_samples.get(sender)?;
            let url = unsubscribe_urls.get(id)?;
            Some(PlannedUnsubscribe {
                sender: sender.clone(),
                url: url.clone(),
                message_count: *count,
            })
        })
        .collect();
    let untouched = inventory
        .message_count
        .saturating_sub(archive.len() + trash.len());

    Ok(CleanupPlan {
        version: 2,
        provider: provider.into(),
        scanned: inventory.message_count,
        indexed: indexed_count,
        conversation_count: inventory.conversation_count,
        unread_count: inventory.unread_count,
        archive,
        trash,
        unsubscribe,
        kept,
        review,
        untouched,
        rule_applied,
        deferred,
        top_bulk_senders,
        created_at: current_time,
    })
}

pub fn preview(plan: &CleanupPlan) -> Value {
    let examples = |mails: &[PlannedMail]| {
        mails
            .iter()
            .take(5)
            .map(|mail| {
                json!({
                    "title": mail.title,
                    "sender": mail.sender,
                    "reason": mail.reason,
                })
            })
            .collect::<Vec<_>>()
    };
    let mut grouped: HashMap<(String, String, String), usize> = HashMap::new();
    for (action, mails) in [("archive", &plan.archive), ("trash", &plan.trash)] {
        for mail in mails {
            *grouped
                .entry((mail.sender.clone(), action.into(), mail.reason.clone()))
                .or_default() += 1;
        }
    }
    let mut action_groups = grouped
        .into_iter()
        .map(|((sender, action, reason), count)| {
            json!({"sender": sender, "action": action, "reason": reason, "count": count})
        })
        .collect::<Vec<_>>();
    action_groups.sort_by(|left, right| {
        right["count"]
            .as_u64()
            .cmp(&left["count"].as_u64())
    });
    action_groups.truncate(12);
    json!({
        "provider": plan.provider,
        "scanned": plan.scanned,
        "indexed": plan.indexed,
        "conversation_count": plan.conversation_count,
        "unread_count": plan.unread_count,
        "archive_count": plan.archive.len(),
        "trash_count": plan.trash.len(),
        "unsubscribe_count": plan.unsubscribe.len(),
        "kept_count": plan.kept,
        "review_count": plan.review,
        "untouched_count": plan.untouched,
        "rule_applied_count": plan.rule_applied,
        "deferred_count": plan.deferred,
        "archive_examples": examples(&plan.archive),
        "trash_examples": examples(&plan.trash),
        "unsubscribe_examples": plan.unsubscribe.iter().map(|entry| json!({
            "sender": entry.sender,
            "message_count": entry.message_count,
        })).collect::<Vec<_>>(),
        "top_bulk_senders": plan.top_bulk_senders,
        "action_groups": action_groups,
    })
}

pub fn report(plan: &CleanupPlan) -> String {
    let service = if plan.provider == "google" {
        "Gmail"
    } else {
        "Outlook"
    };
    format!(
        "Audit {service} terminé : {} messages individuels recensés, {} candidats inspectés, {} proposés à l’archivage, {} à la corbeille, {} désabonnement(s) sécurisé(s), {} protégés après analyse et {} cas ambigus.",
        plan.scanned,
        plan.indexed,
        plan.archive.len(),
        plan.trash.len(),
        plan.unsubscribe.len(),
        plan.kept,
        plan.review,
    )
}

pub fn ids(plan: &CleanupPlan) -> (Vec<String>, Vec<String>) {
    (
        plan.archive.iter().map(|mail| mail.id.clone()).collect(),
        plan.trash.iter().map(|mail| mail.id.clone()).collect(),
    )
}

pub fn unsubscribe_urls(plan: &CleanupPlan) -> Vec<String> {
    plan.unsubscribe
        .iter()
        .map(|entry| entry.url.clone())
        .collect()
}

/// Aligne immédiatement l'index local sur les déplacements distants. Gmail
/// garde les archives cherchables ; les messages à la corbeille et les anciens
/// identifiants Outlook sont retirés jusqu'à la prochaine synchronisation.
pub fn mark_local_after_execution(db: &Db, undo: &Value) -> Result<()> {
    let provider = undo["provider"].as_str().unwrap_or("");
    db.with(|connection| {
        if provider == "google" {
            for id in undo["trashed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                connection.execute(
                    "UPDATE items SET status='removed' WHERE source_ref=?1",
                    [format!("google:gmail:{id}")],
                )?;
            }
        } else if provider == "microsoft" {
            for pair in undo["archived"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(undo["trashed"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
            {
                if let Some((old_id, _)) = pair.split_once('\t') {
                    connection.execute(
                        "UPDATE items SET status='removed' WHERE source_ref=?1",
                        [format!("microsoft:mail:{old_id}")],
                    )?;
                }
            }
        }
        Ok(())
    })
}

pub fn mark_local_after_undo(db: &Db, undo: &Value) -> Result<()> {
    if undo["provider"].as_str() != Some("google") {
        return Ok(());
    }
    db.with(|connection| {
        for id in undo["trashed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            connection.execute(
                "UPDATE items SET status='active' WHERE source_ref=?1",
                [format!("google:gmail:{id}")],
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(title: &str, body: &str, age_days: i64) -> IndexedMail {
        IndexedMail {
            id: "id".into(),
            title: title.into(),
            body: body.into(),
            sender: String::new(),
            received_at: now() - age_days * 86_400,
        }
    }

    #[test]
    fn une_regle_cible_expediteur_et_type_sans_elargir() {
        let rule = crate::rules::MailCleanupRule {
            action: "archive".into(),
            provider: None,
            sender_terms: vec!["anthropic".into()],
            topics: vec!["invoice".into()],
        };
        let mut invoice = mail("Your invoice", "Receipt attached", 2);
        invoice.sender = "Anthropic <billing@anthropic.com>".into();
        let mut marketing = mail("New features", "Newsletter", 2);
        marketing.sender = "Anthropic <news@anthropic.com>".into();
        assert!(rule_matches(&rule, &invoice));
        assert!(!rule_matches(&rule, &marketing));
    }

    #[test]
    fn une_facture_prime_sur_le_pied_de_page_marketing() {
        let value = mail(
            "Votre facture d’électricité",
            "De : fournisseur@example.com\nFacture disponible. Unsubscribe from marketing.",
            500,
        );
        assert_eq!(classify_mail(&value, now()).0, Decision::Keep);
    }

    #[test]
    fn une_vieille_promotion_est_proposee_a_la_corbeille() {
        let value = mail(
            "-50% sur toute la collection",
            "De : newsletter@shop.example\nPromotion. Unsubscribe.",
            500,
        );
        assert_eq!(classify_mail(&value, now()).0, Decision::Trash);
    }

    #[test]
    fn gagner_des_billets_nest_pas_confondu_avec_une_reservation() {
        let value = mail(
            "Win tickets for the new season",
            "De : newsletter@club.example\nMarketing offer. Unsubscribe.",
            500,
        );
        assert_eq!(classify_mail(&value, now()).0, Decision::Trash);
    }

    #[test]
    fn une_notification_recente_ou_un_message_personnel_reste() {
        let recent = mail(
            "Notification",
            "De : no-reply@example.com\nAutomated notification.",
            2,
        );
        let personal = mail(
            "Déjeuner demain ?",
            "De : lea@example.com\nTu es libre ?",
            300,
        );
        assert_eq!(classify_mail(&recent, now()).0, Decision::Keep);
        assert_eq!(classify_mail(&personal, now()).0, Decision::Keep);
    }

    #[test]
    fn le_perimetre_fournisseur_est_ferme() {
        assert_eq!(provider_prefix("google").unwrap(), "google:gmail:");
        assert_eq!(provider_prefix("microsoft").unwrap(), "microsoft:mail:");
        assert!(provider_prefix("apple").is_err());
    }

    #[test]
    fn une_execution_massive_reste_bornee() {
        let item = PlannedMail {
            id: "id".into(),
            title: "Marketing".into(),
            sender: "newsletter@example.com".into(),
            reason: "marketing".into(),
            received_at: 1,
        };
        let mut archive = vec![item.clone(); 1_500];
        let mut trash = vec![item; 1_000];
        let deferred = cap_actions(&mut archive, &mut trash);
        assert_eq!(archive.len() + trash.len(), MAX_ACTIONS_PER_PLAN);
        assert_eq!(deferred, 500);
    }
}
