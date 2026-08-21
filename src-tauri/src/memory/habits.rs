//! Mémoire procédurale : comment l'utilisateur aime que les choses soient faites.
//!
//! Jusqu'ici, Syn ne savait de son utilisateur que ce que celui-ci avait tapé à
//! la main dans les Règles. Tout le reste — le compte qu'il utilise vraiment
//! pour écrire, sa façon d'ouvrir un mail, les heures où il travaille, l'endroit
//! où il range ses factures — était sous les yeux de Syn sans jamais être vu.
//!
//! **Rien n'est appliqué en silence.** Une habitude est d'abord OBSERVÉE (et
//! comptée), puis PROPOSÉE à l'utilisateur, qui la confirme ou la rejette. Seule
//! une habitude confirmée entre dans le prompt comme un fait. Une habitude
//! rejetée reste rejetée, même si Syn la réobserve : ce serait autrement une
//! façon polie d'ignorer un refus.

use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde_json::{json, Value};

/// Nombre d'observations avant qu'une habitude mérite d'être proposée. En
/// dessous, c'est une coïncidence, pas une habitude.
pub const SEUIL_PROPOSITION: i64 = 3;

pub fn observe(db: &Db, topic: &str, subject: &str, value: &str, evidence: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    db.with(|c| {
        c.execute(
            "INSERT INTO preferences (id, topic, subject, value, observations, first_seen, last_seen, status, evidence)
             VALUES (?1,?2,?3,?4,1,?5,?5,'observed',?6)
             ON CONFLICT(topic, subject, value) DO UPDATE SET
               observations = observations + 1,
               last_seen = excluded.last_seen,
               evidence = excluded.evidence",
            params![new_id(), topic, subject, value, now(), evidence],
        )?;
        Ok(())
    })
}

/// Habitude recalculée à chaque passe (et non accumulée) : le rythme de travail
/// se déduit d'une distribution, pas d'un décompte.
fn set_unique(
    db: &Db,
    topic: &str,
    subject: &str,
    value: &str,
    observations: i64,
    evidence: &str,
) -> Result<()> {
    db.with(|c| {
        c.execute(
            "DELETE FROM preferences WHERE topic=?1 AND subject=?2 AND value<>?3 AND status<>'confirmed'",
            params![topic, subject, value],
        )?;
        c.execute(
            "INSERT INTO preferences (id, topic, subject, value, observations, first_seen, last_seen, status, evidence)
             VALUES (?1,?2,?3,?4,?5,?6,?6,'observed',?7)
             ON CONFLICT(topic, subject, value) DO UPDATE SET
               observations = excluded.observations,
               last_seen = excluded.last_seen,
               evidence = excluded.evidence",
            params![new_id(), topic, subject, value, observations, now(), evidence],
        )?;
        Ok(())
    })
}

fn cursor(db: &Db, key: &str) -> i64 {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT value FROM memory_state WHERE key=?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0))
    })
    .unwrap_or(0)
}

fn set_cursor(db: &Db, key: &str, value: i64) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO memory_state (key, value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value.to_string()],
        )?;
        Ok(())
    })
}

/// Une passe d'apprentissage, bornée. Appelée par la boucle de fond.
pub fn learn(db: &Db, budget: usize) -> Result<usize> {
    let mut appris = 0;
    appris += apprend_des_actions(db, budget)?;
    appris += apprend_des_mails_envoyes(db, budget)?;
    appris += apprend_le_rythme(db)?;
    Ok(appris)
}

/// Ce que l'utilisateur a laissé Syn faire : le compte d'envoi qu'il choisit,
/// les dossiers vers lesquels il déplace ses fichiers.
fn apprend_des_actions(db: &Db, budget: usize) -> Result<usize> {
    let from_cursor = cursor(db, "habits.actions");
    let rows: Vec<(String, String, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT tool, input, created_at FROM actions_log
             WHERE status='executed' AND created_at > ?1
             ORDER BY created_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![from_cursor, budget as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut high_water = from_cursor;
    let mut count = 0;
    for (tool, input, created_at) in rows {
        high_water = high_water.max(created_at);
        let args: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
        match tool.as_str() {
            "mail.send" | "mail.draft" => {
                if let Some(via) = args["via"].as_str() {
                    observe(
                        db,
                        "mail.compte",
                        "",
                        compte_lisible(via),
                        "compte utilisé pour tes derniers envois",
                    )?;
                    count += 1;
                }
            }
            "files.move" => {
                let (Some(source), Some(dest)) = (args["from"].as_str(), args["to"].as_str())
                else {
                    continue;
                };
                let extension = std::path::Path::new(source)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let dossier = std::path::Path::new(dest)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                if extension.is_empty() || dossier.is_empty() {
                    continue;
                }
                observe(
                    db,
                    "rangement.destination",
                    &extension,
                    &dossier,
                    "dossier où tu ranges ce type de fichier",
                )?;
                count += 1;
            }
            _ => {}
        }
    }
    set_cursor(db, "habits.actions", high_water)?;
    Ok(count)
}

fn compte_lisible(via: &str) -> &str {
    match via {
        "google" => "Gmail",
        "microsoft" => "Outlook",
        _ => "Apple Mail",
    }
}

/// La façon dont l'utilisateur ouvre et clôt ses messages, lue dans ses propres
/// envois. On ne retient que des formules courtes : une phrase entière n'est pas
/// une habitude, c'est un contenu.
fn apprend_des_mails_envoyes(db: &Db, budget: usize) -> Result<usize> {
    let moi = super::graph::self_addresses(db);
    if moi.is_empty() {
        return Ok(0);
    }
    let from_cursor = cursor(db, "habits.mails");
    let rows: Vec<(String, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT substr(COALESCE(body,''),1,2000), ingested_at FROM items
             WHERE source='mail' AND status='active' AND ingested_at > ?1
             ORDER BY ingested_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![from_cursor, budget as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut high_water = from_cursor;
    let mut count = 0;
    for (body, ingested_at) in rows {
        high_water = high_water.max(ingested_at);
        let (expediteurs, _) = super::graph::parse_headers(&body);
        if !expediteurs
            .iter()
            .any(|(_, address)| moi.iter().any(|m| m == address))
        {
            continue;
        }
        let corps: Vec<&str> = body
            .split("\n\n")
            .nth(1)
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(ouverture) = corps.first().filter(|l| formule(l)) {
            observe(
                db,
                "mail.ouverture",
                "",
                ouverture,
                "façon dont tu commences tes messages",
            )?;
            count += 1;
        }
        if corps.len() > 1 {
            if let Some(cloture) = corps
                .iter()
                .rev()
                .find(|l| formule(l) && !l.starts_with("Bonjour"))
            {
                observe(
                    db,
                    "mail.cloture",
                    "",
                    cloture,
                    "façon dont tu termines tes messages",
                )?;
                count += 1;
            }
        }
    }
    set_cursor(db, "habits.mails", high_water)?;
    Ok(count)
}

/// Une formule tient sur une ligne courte et ne contient pas de ponctuation de
/// phrase : « Bien à toi, » oui ; « Je te confirme notre rendez-vous. » non.
fn formule(ligne: &str) -> bool {
    let ligne = ligne.trim();
    let mots = ligne.split_whitespace().count();
    !ligne.is_empty()
        && ligne.chars().count() <= 40
        && mots <= 6
        && !ligne.contains('.')
        && !ligne.contains('?')
        && !ligne.contains('@')
        && !ligne.contains("http")
}

/// Les heures où l'utilisateur s'adresse à Syn : c'est ce qui permettra plus
/// tard de ne pas le déranger hors de son rythme réel.
fn apprend_le_rythme(db: &Db) -> Result<usize> {
    let heures: Vec<i64> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT created_at FROM conversations
             WHERE role='user' AND created_at >= ?1 ORDER BY created_at DESC LIMIT 500",
        )?;
        let rows = stmt.query_map(params![now() - 30 * 86_400], |r| r.get::<_, i64>(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    if heures.len() < 20 {
        return Ok(0); // pas assez de matière pour prétendre connaître un rythme
    }
    use chrono::Timelike;
    let mut locales: Vec<u32> = heures
        .iter()
        .filter_map(|at| chrono::DateTime::from_timestamp(*at, 0))
        .map(|dt| dt.with_timezone(&chrono::Local).hour())
        .collect();
    locales.sort_unstable();
    let debut = locales[locales.len() / 10];
    let fin = locales[locales.len() * 9 / 10];
    set_unique(
        db,
        "rythme.heures",
        "",
        &format!("{debut}h–{fin}h"),
        locales.len() as i64,
        "heures auxquelles tu t'adresses à Syn",
    )?;
    Ok(1)
}

// ————— Lecture et arbitrage par l'utilisateur —————

fn rows_to_json(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Value>> {
    let rows = stmt.query_map(params, |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "topic": r.get::<_, String>(1)?,
            "subject": r.get::<_, String>(2)?,
            "value": r.get::<_, String>(3)?,
            "observations": r.get::<_, i64>(4)?,
            "last_seen": r.get::<_, i64>(5)?,
            "status": r.get::<_, String>(6)?,
            "evidence": r.get::<_, Option<String>>(7)?,
        }))
    })?;
    let mut out = vec![];
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Ce que Syn a remarqué et qui attend l'avis de l'utilisateur.
pub fn pending(db: &Db) -> Result<Vec<Value>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, topic, subject, value, observations, last_seen, status, evidence
             FROM preferences WHERE status='observed' AND observations >= ?1
             ORDER BY observations DESC, last_seen DESC LIMIT 20",
        )?;
        Ok(rows_to_json(&mut stmt, params![SEUIL_PROPOSITION])?)
    })
}

pub fn confirmed(db: &Db) -> Result<Vec<Value>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, topic, subject, value, observations, last_seen, status, evidence
             FROM preferences WHERE status='confirmed' ORDER BY topic, observations DESC LIMIT 40",
        )?;
        Ok(rows_to_json(&mut stmt, [])?)
    })
}

pub fn list_all(db: &Db) -> Result<Vec<Value>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, topic, subject, value, observations, last_seen, status, evidence
             FROM preferences WHERE status='confirmed' OR observations >= ?1
             ORDER BY status='confirmed' DESC, observations DESC LIMIT 60",
        )?;
        Ok(rows_to_json(&mut stmt, params![SEUIL_PROPOSITION])?)
    })
}

/// L'utilisateur tranche. Un rejet est définitif tant qu'il ne revient pas
/// dessus : réobserver l'habitude ne la remettra pas en file.
pub fn decide(db: &Db, id: &str, accepte: bool) -> Result<()> {
    db.with(|c| {
        c.execute(
            "UPDATE preferences SET status=?2 WHERE id=?1",
            params![id, if accepte { "confirmed" } else { "rejected" }],
        )?;
        Ok(())
    })
}

/// Valeur confirmée pour un sujet donné (le seul canal par lequel une habitude
/// influence réellement le comportement de Syn).
pub fn confirmed_value(db: &Db, topic: &str, subject: &str) -> Option<String> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT value FROM preferences
             WHERE topic=?1 AND subject=?2 AND status='confirmed'
             ORDER BY observations DESC LIMIT 1",
            params![topic, subject],
            |r| r.get::<_, String>(0),
        )
        .ok())
    })
    .unwrap_or(None)
}

/// Le compte d'envoi que l'utilisateur a CONFIRMÉ comme étant le sien.
///
/// C'est le seul endroit où une habitude change ce que Syn fait plutôt que ce
/// qu'il sait — et elle ne le fait qu'après validation explicite. Le compte
/// retenu reste affiché sur la carte de confirmation avant tout envoi : c'est
/// une question qu'on cesse de poser, pas une décision qu'on cache.
pub fn compte_denvoi_confirme(db: &Db) -> Option<&'static str> {
    match confirmed_value(db, "mail.compte", "")?.as_str() {
        "Gmail" => Some("google"),
        "Outlook" => Some("microsoft"),
        "Apple Mail" => Some("apple"),
        _ => None,
    }
}

fn phrase(topic: &str, subject: &str, value: &str) -> String {
    match topic {
        "mail.compte" => format!("Tu envoies tes mails depuis {value}."),
        "mail.ouverture" => format!("Tu ouvres tes messages par « {value} »."),
        "mail.cloture" => format!("Tu termines tes messages par « {value} »."),
        "rythme.heures" => format!("Tu travailles surtout entre {value}."),
        "rangement.destination" => {
            format!("Tu ranges tes fichiers .{subject} dans {value}.")
        }
        _ => format!("{topic} : {value}"),
    }
}

/// Bloc injecté dans le system prompt. Uniquement des habitudes CONFIRMÉES :
/// une observation non validée n'a pas à influencer une réponse.
pub fn summary_for_prompt(db: &Db) -> String {
    let Ok(list) = confirmed(db) else {
        return String::new();
    };
    if list.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n— Habitudes de l'utilisateur (confirmées par lui) —\n");
    for pref in list.iter().take(12) {
        out.push_str(&format!(
            "{}\n",
            phrase(
                pref["topic"].as_str().unwrap_or(""),
                pref["subject"].as_str().unwrap_or(""),
                pref["value"].as_str().unwrap_or(""),
            )
        ));
    }
    out
}

/// Formulation lisible d'une habitude, pour l'interface.
pub fn describe(pref: &Value) -> String {
    phrase(
        pref["topic"].as_str().unwrap_or(""),
        pref["subject"].as_str().unwrap_or(""),
        pref["value"].as_str().unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Db {
        let dir = std::env::temp_dir().join(format!("syn-habits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db"), &"c".repeat(64)).unwrap()
    }

    fn envoi(db: &Db, via: &str, at: i64) {
        db.with(|c| {
            c.execute(
                "INSERT INTO actions_log (id,tool,input,risk_class,status,created_at)
                 VALUES (?1,'mail.send',?2,'floor','executed',?3)",
                params![new_id(), json!({"via": via}).to_string(), at],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn une_habitude_nest_proposee_quapres_plusieurs_observations() {
        let db = base();
        envoi(&db, "google", 1_700_000_001);
        envoi(&db, "google", 1_700_000_002);
        learn(&db, 100).unwrap();
        assert!(
            pending(&db).unwrap().is_empty(),
            "deux fois n'est pas une habitude"
        );
        envoi(&db, "google", 1_700_000_003);
        learn(&db, 100).unwrap();
        let proposees = pending(&db).unwrap();
        assert_eq!(proposees.len(), 1);
        assert_eq!(proposees[0]["value"], "Gmail");
    }

    /// Une habitude rejetée ne doit pas revenir par la porte des observations.
    #[test]
    fn un_rejet_tient_meme_si_syn_reobserve() {
        let db = base();
        for i in 0..3 {
            envoi(&db, "google", 1_700_000_000 + i);
        }
        learn(&db, 100).unwrap();
        let id = pending(&db).unwrap()[0]["id"].as_str().unwrap().to_string();
        decide(&db, &id, false).unwrap();
        for i in 10..14 {
            envoi(&db, "google", 1_700_000_000 + i);
        }
        learn(&db, 100).unwrap();
        assert!(pending(&db).unwrap().is_empty());
        assert!(confirmed_value(&db, "mail.compte", "").is_none());
    }

    #[test]
    fn seules_les_habitudes_confirmees_entrent_dans_le_prompt() {
        let db = base();
        for i in 0..3 {
            envoi(&db, "microsoft", 1_700_000_000 + i);
        }
        learn(&db, 100).unwrap();
        assert!(summary_for_prompt(&db).is_empty());
        let id = pending(&db).unwrap()[0]["id"].as_str().unwrap().to_string();
        decide(&db, &id, true).unwrap();
        assert!(summary_for_prompt(&db).contains("Outlook"));
        assert_eq!(
            confirmed_value(&db, "mail.compte", ""),
            Some("Outlook".into())
        );
    }

    /// Tant que l'utilisateur n'a pas confirmé, Syn continue de demander son
    /// compte d'envoi : observer n'est pas décider.
    #[test]
    fn le_compte_denvoi_ne_sapplique_quapres_confirmation() {
        let db = base();
        for i in 0..4 {
            envoi(&db, "google", 1_700_000_000 + i);
        }
        learn(&db, 100).unwrap();
        assert_eq!(compte_denvoi_confirme(&db), None);

        let id = pending(&db).unwrap()[0]["id"].as_str().unwrap().to_string();
        decide(&db, &id, true).unwrap();
        assert_eq!(compte_denvoi_confirme(&db), Some("google"));
    }

    #[test]
    fn une_phrase_entiere_nest_pas_une_formule() {
        assert!(formule("Bien à toi,"));
        assert!(formule("Bonjour Julie,"));
        assert!(!formule("Je te confirme notre rendez-vous de mardi."));
        assert!(!formule("paul@moi.fr"));
    }
}
