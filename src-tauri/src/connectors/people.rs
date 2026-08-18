//! Connecteur Personnes (doc Connecteurs §4) — V1 hors biométrie.
//! Import initial (onboarding) + apprentissage progressif (demande groupée).
//! Graphe social de tiers non-consentants → strictement local, chiffré, effaçable.

use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde_json::{json, Value};

/// Aperçu via l'API Contacts officielle : aucune lecture directe de la base privée.
pub fn os_contacts_preview() -> Result<Vec<Value>> {
    let mut out = crate::connectors::native::contacts()?;
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out.dedup_by(|a, b| a["name"] == b["name"]);
    Ok(out)
}

pub fn import_person(
    db: &Db,
    name: &str,
    relationship: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    birthday: Option<&str>,
) -> Result<String> {
    let id = crate::memory::find_or_create_person(db, name, email, phone)?;
    db.with(|c| {
        if let Some(rel) = relationship {
            c.execute(
                "UPDATE people SET relationship=?2 WHERE id=?1",
                params![id, rel],
            )?;
        }
        if let Some(b) = birthday {
            c.execute("UPDATE people SET birthday=?2 WHERE id=?1", params![id, b])?;
        }
        Ok(())
    })?;
    crate::security::log_access(db, "people", "import", Some(name));
    Ok(id)
}

pub fn list_people(db: &Db) -> Result<Vec<Value>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, relationship, comm_channels, last_interaction, birthday FROM people ORDER BY name LIMIT 500",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "relationship": r.get::<_, Option<String>>(2)?,
                "comm_channels": r.get::<_, Option<String>>(3)?
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                "last_interaction": r.get::<_, Option<i64>>(4)?,
                "birthday": r.get::<_, Option<String>>(5)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// `people.context` : contexte disponible via les canaux accessibles UNIQUEMENT.
/// Alimente un brouillon, jamais un envoi automatique (plancher).
pub fn context(db: &Db, name: &str) -> Result<Value> {
    db.with(|c| {
        let person: Option<(String, String, Option<String>, Option<String>)> = c
            .query_row(
                "SELECT id, name, relationship, comm_channels FROM people
                 WHERE lower(name) LIKE '%'||lower(?1)||'%'
                    OR lower(COALESCE(relationship,'')) = lower(?1) LIMIT 1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((id, pname, relationship, channels)) = person else {
            return Ok(json!({"found": false, "note": format!("Personne « {name} » inconnue. L'utilisateur peut l'ajouter dans Apprendre à Syn.")}));
        };
        let mut linked: Vec<Value> = vec![];
        let mut stmt = c.prepare(
            "SELECT i.title, i.type, i.source_ref, i.mtime FROM person_links pl
             JOIN items i ON i.id = pl.item_id WHERE pl.person_id = ?1 AND i.status='active'
             ORDER BY i.mtime DESC LIMIT 10",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(json!({
                "title": r.get::<_, Option<String>>(0)?,
                "type": r.get::<_, String>(1)?,
                "source_ref": r.get::<_, String>(2)?,
                "mtime": r.get::<_, Option<i64>>(3)?,
            }))
        })?;
        for r in rows {
            linked.push(r?);
        }
        Ok(json!({
            "found": true,
            "name": pname,
            "relationship": relationship,
            "comm_channels": channels.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            "linked_items": linked,
        }))
    })
}

/// Correspondants connus par les mails déjà synchronisés (Gmail, Outlook,
/// Apple Mail). Sans cela, Syn ignorait des adresses qu'il avait pourtant sous
/// les yeux : l'utilisateur devait lui dicter une adresse figurant dans sa
/// propre boîte de réception.
///
/// Les en-têtes ingérés ont la forme `De : Nom <adresse>` / `À : …`. On lit ces
/// deux lignes uniquement — jamais le corps, qui n'est pas une source d'identité.
fn correspondents_matching(db: &Db, folded_name: &str) -> Result<Vec<Value>> {
    let bodies: Vec<String> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT COALESCE(body,'') FROM items
             WHERE source='mail' AND status='active'
               AND syn_fold(COALESCE(body,'')) LIKE '%'||?1||'%'
             ORDER BY COALESCE(created_at,0) DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(params![folded_name], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;

    let mut seen = std::collections::HashSet::new();
    let mut matches = Vec::new();
    for body in bodies {
        for line in body.lines().take(4) {
            let Some(list) = line
                .strip_prefix("De : ")
                .or_else(|| line.strip_prefix("À : "))
                .or_else(|| line.strip_prefix("A : "))
            else {
                continue;
            };
            for entry in list.split(',') {
                let Some((display, email)) = split_address(entry) else {
                    continue;
                };
                let folded_display = crate::db::fold(&display);
                let local = email.split('@').next().unwrap_or_default();
                if !folded_display.contains(folded_name)
                    && !crate::db::fold(local).contains(folded_name)
                {
                    continue;
                }
                if !seen.insert(email.clone()) {
                    continue;
                }
                matches.push(json!({
                    "name": if display.is_empty() { email.clone() } else { display },
                    "email": email,
                    "source": "mails synchronisés",
                }));
            }
        }
    }
    Ok(matches)
}

/// Découpe `Nom <adresse>` ou une adresse nue. Rend `(nom, adresse)`.
fn split_address(entry: &str) -> Option<(String, String)> {
    let entry = entry.trim();
    if let Some(open) = entry.rfind('<') {
        let email = entry[open + 1..].trim_end_matches('>').trim().to_lowercase();
        let display = entry[..open].trim().trim_matches('"').to_string();
        return valid_email(&email).then_some((display, email));
    }
    let email = entry.trim_matches('"').to_lowercase();
    valid_email(&email).then_some((String::new(), email))
}

fn valid_email(value: &str) -> bool {
    value.contains('@') && value.contains('.') && !value.contains(' ')
}

/// Résolution explicite d'un destinataire. Renvoie toutes les correspondances
/// afin que le modèle demande une précision en cas d'homonymie ou d'adresses
/// multiples, au lieu de choisir ou de fabriquer une adresse.
///
/// Deux sources, dans cet ordre : le carnet d'adresses explicite, puis les
/// mails déjà synchronisés.
pub fn resolve_email(db: &Db, name: &str) -> Result<Value> {
    let folded = crate::db::fold(name.trim());
    if folded.is_empty() {
        return Ok(
            json!({"resolved": false, "matches": [], "note": "Nom du destinataire manquant."}),
        );
    }
    let matches = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, COALESCE(comm_channels,''), COALESCE(aliases,''), COALESCE(relationship,'')
             FROM people
             WHERE syn_fold(name) LIKE '%'||?1||'%'
                OR syn_fold(COALESCE(aliases,'')) LIKE '%'||?1||'%'
                OR syn_fold(COALESCE(relationship,'')) = ?1
             ORDER BY CASE WHEN syn_fold(name)=?1 THEN 0 ELSE 1 END, name LIMIT 10",
        )?;
        let rows = stmt.query_map(params![folded], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, person_name, channels) = row?;
            let parsed: Value = serde_json::from_str(&channels).unwrap_or(Value::Null);
            for email in parsed["emails"].as_array().cloned().unwrap_or_default() {
                if let Some(email) = email.as_str().filter(|e| e.contains('@') && e.contains('.')) {
                    out.push(json!({"person_id": id, "name": person_name, "email": email}));
                }
            }
        }
        Ok(out)
    })?;
    // Le carnet d'adresses explicite prime ; les mails déjà synchronisés
    // complètent quand il ne dit rien.
    let mut matches = matches;
    if matches.is_empty() {
        matches = correspondents_matching(db, &folded)?;
    }
    let resolved = matches.len() == 1;
    Ok(json!({
        "resolved": resolved,
        "matches": matches,
        "note": if resolved {
            "Une adresse unique a été trouvée. Utilise exactement celle-ci."
        } else {
            "Aucune adresse unique : demande à l'utilisateur de préciser le destinataire ou l'adresse."
        }
    }))
}

/// Vérifie qu'une adresse produite par le modèle appartient réellement à une
/// personne nommée par l'utilisateur au cours de la conversation.
pub fn email_is_known_for_mentioned_person(
    db: &Db,
    email: &str,
    trusted_user_text: &str,
) -> Result<bool> {
    let wanted = email.trim().to_lowercase();
    let user = crate::db::fold(trusted_user_text);
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT name, COALESCE(aliases,''), COALESCE(relationship,''), COALESCE(comm_channels,'') FROM people",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (name, aliases, relationship, channels) = row?;
            let parsed: Value = serde_json::from_str(&channels).unwrap_or(Value::Null);
            let owns_email = parsed["emails"]
                .as_array()
                .is_some_and(|emails| emails.iter().any(|v| {
                    v.as_str().is_some_and(|candidate| candidate.eq_ignore_ascii_case(&wanted))
                }));
            if !owns_email {
                continue;
            }
            let name_is_mentioned = [name, relationship]
                .iter()
                .map(|s| crate::db::fold(s))
                .any(|s| s.chars().count() >= 3 && user.contains(&s));
            let alias_is_mentioned = serde_json::from_str::<Value>(&aliases)
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .map(crate::db::fold)
                .any(|s| s.chars().count() >= 3 && user.contains(&s));
            if name_is_mentioned || alias_is_mentioned {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

pub fn pending_unknowns(db: &Db) -> Result<Vec<Value>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, context, source_ref, created_at FROM unknown_names
             WHERE status='pending' ORDER BY created_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "context": r.get::<_, Option<String>>(2)?,
                "source_ref": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, i64>(4)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn label_unknown(
    db: &Db,
    unknown_id: &str,
    name: &str,
    relationship: Option<&str>,
) -> Result<()> {
    let pid = crate::memory::find_or_create_person(db, name, None, None)?;
    db.with(|c| {
        if let Some(rel) = relationship {
            c.execute(
                "UPDATE people SET relationship=?2 WHERE id=?1",
                params![pid, rel],
            )?;
        }
        c.execute(
            "UPDATE unknown_names SET status='labeled' WHERE id=?1",
            params![unknown_id],
        )?;
        Ok(())
    })
}

pub fn add_fact_about(db: &Db, text: &str) -> Result<String> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
             VALUES (?1, 'conversation', ?2, 'fact', ?3, ?4, ?5, 'active')",
            params![
                id,
                format!("fact:{id}"),
                text.chars().take(80).collect::<String>(),
                text,
                now()
            ],
        )?;
        Ok(())
    })?;
    Ok(id)
}

#[cfg(test)]
mod correspondents_tests {
    use super::*;

    /// Un correspondant présent dans les mails synchronisés doit être trouvé
    /// sans que l'utilisateur ait à saisir son adresse à la main : c'est
    /// exactement ce que Paul reprochait le 17/08/2026.
    #[test]
    fn une_adresse_vue_dans_les_mails_synchronises_est_resolue() {
        let dir = std::env::temp_dir().join(format!("syn-corresp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"6".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO items(id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES ('m1','mail','google:gmail:1','email','Devis',
                         'De : Camille Roux <camille.roux@exemple.fr>\nÀ : moi@exemple.fr\nObjet : Devis\n\nBonjour',
                         10, 10, 'active')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let resolved = resolve_email(&db, "Camille").unwrap();
        assert_eq!(resolved["resolved"], true, "{resolved}");
        assert_eq!(resolved["matches"][0]["email"], "camille.roux@exemple.fr");

        // Un inconnu reste inconnu : on ne fabrique jamais d'adresse.
        let unknown = resolve_email(&db, "Bertrand").unwrap();
        assert_eq!(unknown["resolved"], false, "{unknown}");
        assert!(unknown["matches"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
