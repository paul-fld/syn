//! La toile : qui est relié à quoi, depuis quand, par quelle observation.
//!
//! Le modèle comprend le langage ; le graphe garantit les faits. Ce module est
//! du côté « garantit » : chaque arête est explicite, datée, comptée et sourcée,
//! donc vérifiable et effaçable — exactement ce qu'un réseau de neurones ne
//! sait pas offrir.
//!
//! Deux principes tiennent tout le reste :
//!
//! 1. **Rien n'est inventé.** Une arête n'existe que si une donnée déjà ingérée
//!    l'atteste (un en-tête de mail, une liste d'invités, un document confié).
//!    Le graphe est donc DÉRIVÉ : on peut le vider et le reconstruire.
//! 2. **Peu de liens, mais typés.** « Tout relier à tout » ne produit que du
//!    bruit : ce qui donne la puissance, c'est un petit nombre de relations
//!    nommées, chacune avec sa date et son nombre d'observations.

use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Nœud du graphe. `self` est l'utilisateur lui-même — le centre de la toile.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Node {
    pub kind: String,
    pub id: String,
}

impl Node {
    pub fn new(kind: &str, id: &str) -> Self {
        Node {
            kind: kind.into(),
            id: id.into(),
        }
    }
    pub fn moi() -> Self {
        Node::new("self", "moi")
    }
}

/// Enregistre (ou renforce) une arête. Le compteur d'observations est ce qui
/// sépare un correspondant quotidien d'un inconnu croisé une fois.
///
/// Les passes de construction écrivent par milliers : elles passent par la
/// variante `_on`, qui partage une seule transaction. Une transaction par lien
/// ferait de la construction de la toile une opération plus lourde que
/// l'indexation qu'elle accompagne.
pub fn observe(
    db: &Db,
    src: &Node,
    kind: &str,
    dst: &Node,
    seen_at: i64,
    origin: &str,
) -> Result<()> {
    db.with(|c| observe_on(c, src, kind, dst, seen_at, origin))
}

fn observe_on(
    c: &rusqlite::Connection,
    src: &Node,
    kind: &str,
    dst: &Node,
    seen_at: i64,
    origin: &str,
) -> Result<()> {
    let seen_at = if seen_at > 0 { seen_at } else { now() };
    c.execute(
        "INSERT INTO relations
           (id, src_kind, src_id, kind, dst_kind, dst_id, observations, first_seen, last_seen, origin)
         VALUES (?1,?2,?3,?4,?5,?6,1,?7,?7,?8)
         ON CONFLICT(src_kind, src_id, kind, dst_kind, dst_id) DO UPDATE SET
           observations = observations + 1,
           first_seen = min(first_seen, excluded.first_seen),
           last_seen  = max(last_seen, excluded.last_seen)",
        params![
            new_id(),
            src.kind,
            src.id,
            kind,
            dst.kind,
            dst.id,
            seen_at,
            origin
        ],
    )?;
    Ok(())
}

/// Correspondant vu dans un en-tête : on retient son adresse et son nom
/// affiché, sans l'inscrire d'office au carnet de l'utilisateur.
pub fn note_contact(db: &Db, address: &str, display_name: &str, seen_at: i64) -> Result<()> {
    db.with(|c| note_contact_on(c, address, display_name, seen_at))
}

fn note_contact_on(
    c: &rusqlite::Connection,
    address: &str,
    display_name: &str,
    seen_at: i64,
) -> Result<()> {
    let address = address.trim().to_lowercase();
    if address.is_empty() {
        return Ok(());
    }
    let name = display_name.trim();
    let seen_at = if seen_at > 0 { seen_at } else { now() };
    c.execute(
        "INSERT INTO contacts (address, display_name, observations, first_seen, last_seen)
         VALUES (?1,?2,1,?3,?3)
         ON CONFLICT(address) DO UPDATE SET
           observations = observations + 1,
           last_seen = max(last_seen, excluded.last_seen),
           display_name = COALESCE(NULLIF(excluded.display_name,''), display_name)",
        params![address, name, seen_at],
    )?;
    Ok(())
}

// ————— Curseurs de construction —————

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

// ————— Adresses de l'utilisateur —————

/// Les adresses de l'utilisateur.
///
/// Le seul signe fiable dans les en-têtes est la PRÉSENCE : l'utilisateur
/// figure dans presque tous les messages de sa propre boîte, ses
/// correspondants dans une minorité. Un critère plus simple — « une adresse
/// qui écrit et reçoit » — désignait les deux côtés de toute conversation
/// suivie, donc n'importe quel proche.
///
/// Quand aucun candidat ne se détache nettement, Syn ne tranche PAS : il rend
/// une liste vide, les fonctions qui en dépendent se taisent, et l'utilisateur
/// confirme lui-même son adresse (`list_identity_candidates`). Deviner ici
/// reviendrait à confondre « il m'a écrit » et « je lui ai écrit ».
pub fn self_addresses(db: &Db) -> Vec<String> {
    let confirmed: Vec<String> = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT address FROM self_identities WHERE confirmed=1 ORDER BY observations DESC LIMIT 12",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .unwrap_or_default();
    if !confirmed.is_empty() {
        return confirmed;
    }

    let echantillon = sample_size(db);
    if echantillon < 5 {
        return vec![];
    }
    let candidats: Vec<(String, i64)> = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT address, observations FROM self_identities ORDER BY observations DESC LIMIT 4",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .unwrap_or_default();
    let Some((address, presence)) = candidats.first() else {
        return vec![];
    };
    let majoritaire = *presence * 10 >= echantillon * 6;
    let detache = candidats
        .get(1)
        .map(|(_, second)| *second * 2 <= *presence)
        .unwrap_or(true);
    if majoritaire && detache {
        vec![address.clone()]
    } else {
        vec![]
    }
}

fn sample_size(db: &Db) -> i64 {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT value FROM memory_state WHERE key='identities.sample'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0))
    })
    .unwrap_or(0)
}

/// Les adresses qui pourraient être celles de l'utilisateur, à lui faire
/// trancher quand Syn n'a pas de majorité nette.
pub fn list_identity_candidates(db: &Db) -> Result<Vec<Value>> {
    let echantillon = sample_size(db).max(1);
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT address, observations, confirmed FROM self_identities
             ORDER BY confirmed DESC, observations DESC LIMIT 6",
        )?;
        let rows = stmt.query_map([], |r| {
            let presence: i64 = r.get(1)?;
            Ok(json!({
                "address": r.get::<_, String>(0)?,
                "observations": presence,
                "presence_pct": (presence * 100 / echantillon).min(100),
                "confirmed": r.get::<_, i64>(2)? != 0,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn is_self(db: &Db, address: &str) -> bool {
    let address = address.to_lowercase();
    self_addresses(db).iter().any(|a| *a == address)
}

/// Recompte les adresses candidates sur un échantillon récent d'en-têtes.
fn refresh_identities(db: &Db) -> Result<()> {
    let headers: Vec<String> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT substr(COALESCE(body,''), 1, 600) FROM items
             WHERE source='mail' AND status='active'
             ORDER BY COALESCE(created_at, ingested_at) DESC LIMIT 400",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;

    // Présence : dans combien de messages distincts l'adresse figure-t-elle,
    // d'un côté ou de l'autre de l'en-tête.
    let mut presence: HashMap<String, i64> = HashMap::new();
    for body in &headers {
        let (from, to) = parse_headers(body);
        let mut vues: Vec<String> = from
            .into_iter()
            .chain(to)
            .map(|(_, address)| address)
            .collect();
        vues.sort();
        vues.dedup();
        for address in vues {
            *presence.entry(address).or_insert(0) += 1;
        }
    }
    if headers.len() < 5 {
        return Ok(());
    }
    for (address, vues) in presence {
        if vues < 3 {
            continue;
        }
        db.with(|c| {
            c.execute(
                "INSERT INTO self_identities (address, observations, confirmed, updated_at)
                 VALUES (?1,?2,0,?3)
                 ON CONFLICT(address) DO UPDATE SET
                   observations = excluded.observations,
                   updated_at = excluded.updated_at",
                params![address, vues, now()],
            )?;
            Ok(())
        })?;
    }
    db.with(|c| {
        c.execute(
            "INSERT INTO memory_state (key, value) VALUES ('identities.sample', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![headers.len().to_string()],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// L'utilisateur corrige : cette adresse est (ou n'est pas) la sienne.
pub fn set_self_address(db: &Db, address: &str, mine: bool) -> Result<()> {
    let address = address.trim().to_lowercase();
    db.with(|c| {
        if mine {
            c.execute(
                "INSERT INTO self_identities (address, observations, confirmed, updated_at)
                 VALUES (?1, 99, 1, ?2)
                 ON CONFLICT(address) DO UPDATE SET confirmed=1, updated_at=excluded.updated_at",
                params![address, now()],
            )?;
        } else {
            c.execute(
                "DELETE FROM self_identities WHERE address=?1",
                params![address],
            )?;
        }
        Ok(())
    })
}

pub fn list_self_addresses(db: &Db) -> Result<Vec<Value>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT address, observations, confirmed FROM self_identities
             ORDER BY confirmed DESC, observations DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "address": r.get::<_, String>(0)?,
                "observations": r.get::<_, i64>(1)?,
                "confirmed": r.get::<_, i64>(2)? != 0,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

// ————— Lecture des en-têtes ingérés —————

/// Extrait `(expéditeurs, destinataires)` des quatre premières lignes d'un mail
/// ingéré. Le corps n'est JAMAIS lu ici : ce n'est pas une source d'identité,
/// et c'est un vecteur d'injection connu.
pub fn parse_headers(body: &str) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut from = vec![];
    let mut to = vec![];
    for line in body.lines().take(4) {
        let (target, list) = if let Some(rest) = line.strip_prefix("De : ") {
            (&mut from, rest)
        } else if let Some(rest) = line
            .strip_prefix("À : ")
            .or_else(|| line.strip_prefix("A : "))
        {
            (&mut to, rest)
        } else {
            continue;
        };
        for entry in list.split(',') {
            if let Some(pair) = crate::connectors::people::split_address(entry) {
                target.push(pair);
            }
        }
    }
    (from, to)
}

// ————— Construction incrémentale —————

/// Une passe de construction, bornée par `budget` items.
///
/// Appelée par la boucle de fond sous le même budget que l'indexation : la
/// toile converge en arrière-plan sans jamais faire attendre l'interactif.
pub fn build(db: &Db, budget: usize) -> Result<usize> {
    refresh_identities(db)?;
    let moi = self_addresses(db);
    let mut done = 0;
    done += build_mail(db, budget, &moi)?;
    done += build_calendar(db)?;
    done += build_person_links(db, budget)?;
    done += build_project_documents(db, budget)?;
    Ok(done)
}

/// Carnet d'adresses en mémoire : résoudre chaque adresse par une requête
/// coûtait un balayage de toute la table `people` par mail traité.
fn address_book(db: &Db) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let rows: Vec<(String, Option<String>)> = db
        .read(|c| {
            let mut stmt = c.prepare("SELECT id, comm_channels FROM people")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .unwrap_or_default();
    for (id, channels) in rows {
        let Some(channels) = channels else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&channels) else {
            continue;
        };
        for email in value["emails"].as_array().cloned().unwrap_or_default() {
            if let Some(email) = email.as_str() {
                map.insert(email.to_lowercase(), id.clone());
            }
        }
    }
    map
}

/// Un correspondant devient un nœud `person` s'il est au carnet, sinon un
/// nœud `contact` porté par son adresse.
fn node_for(address: &str, book: &HashMap<String, String>) -> Node {
    match book.get(address) {
        Some(person_id) => Node::new("person", person_id),
        None => Node::new("contact", address),
    }
}

fn build_mail(db: &Db, budget: usize, moi: &[String]) -> Result<usize> {
    let from_cursor = cursor(db, "graph.mail");
    let rows: Vec<(String, String, i64, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, substr(COALESCE(body,''),1,600), COALESCE(created_at, ingested_at), ingested_at
             FROM items
             WHERE source='mail' AND status='active' AND ingested_at > ?1
             ORDER BY ingested_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![from_cursor, budget as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
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

    let book = address_book(db);
    let mut high_water = from_cursor;
    let mut count = 0;
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for (item_id, body, when, ingested_at) in &rows {
            let (when, ingested_at) = (*when, *ingested_at);
            high_water = high_water.max(ingested_at);
            let (from, to) = parse_headers(body);
            let item = Node::new("item", item_id);

            for (name, address) in &from {
                note_contact_on(&tx, address, name, when)?;
                let sender = node_for(address, &book);
                observe_on(&tx, &sender, "auteur_de", &item, when, "mail")?;
                if !moi.iter().any(|a| a == address) {
                    observe_on(&tx, &sender, "ecrit_a", &Node::moi(), when, "mail")?;
                }
            }

            let expediteur_est_moi = from.iter().any(|(_, a)| moi.iter().any(|m| m == a));
            // Une liste de diffusion n'apprend rien sur les liens réels : au-delà
            // de cinq destinataires, on ne retient pas les liens entre eux.
            let echange_restreint = to.len() <= 5;
            for (name, address) in &to {
                if moi.iter().any(|m| m == address) {
                    continue;
                }
                note_contact_on(&tx, address, name, when)?;
                let destinataire = node_for(address, &book);
                if expediteur_est_moi {
                    observe_on(&tx, &Node::moi(), "ecrit_a", &destinataire, when, "mail")?;
                }
                if echange_restreint {
                    for (_, autre) in &to {
                        if autre <= address || moi.iter().any(|m| m == autre) {
                            continue;
                        }
                        observe_on(
                            &tx,
                            &destinataire,
                            "co_destinataire",
                            &node_for(autre, &book),
                            when,
                            "mail",
                        )?;
                    }
                }
            }
            count += 1;
        }
        tx.commit()?;
        Ok(())
    })?;
    set_cursor(db, "graph.mail", high_water)?;
    Ok(count)
}

/// Les invités d'un rendez-vous sont un lien social factuel : « nous étions
/// dans la même réunion ». Fenêtre glissante (±120 jours) plutôt que curseur :
/// un événement peut être déplacé ou annulé après coup.
fn build_calendar(db: &Db) -> Result<usize> {
    let rows: Vec<(String, String, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, COALESCE(attendees,'[]'), \"start\" FROM events
             WHERE \"start\" BETWEEN ?1 AND ?2 AND attendees IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![now() - 120 * 86_400, now() + 120 * 86_400], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let book = address_book(db);
    let mut count = 0;
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for (event_id, attendees, start) in &rows {
            let start = *start;
            let Ok(list) = serde_json::from_str::<Value>(attendees) else {
                continue;
            };
            let Some(list) = list.as_array() else { continue };
            let event = Node::new("event", event_id);
            for entry in list {
                let address = entry["email"]
                    .as_str()
                    .or_else(|| entry.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                if !address.contains('@') {
                    continue;
                }
                let name = entry["name"].as_str().unwrap_or("");
                note_contact_on(&tx, &address, name, start)?;
                // Les invités d'un même rendez-vous sont déjà reliés par
                // l'événement : inutile de multiplier les arêtes entre eux.
                observe_on(
                    &tx,
                    &event,
                    "reunit",
                    &node_for(&address, &book),
                    start,
                    "calendar",
                )?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    Ok(count)
}

/// Miroir des liens personne ↔ document déjà établis par les connecteurs.
fn build_person_links(db: &Db, budget: usize) -> Result<usize> {
    let from_cursor = cursor(db, "graph.person_links");
    let rows: Vec<(i64, String, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT pl.rowid, pl.person_id, pl.item_id FROM person_links pl
             WHERE pl.rowid > ?1 ORDER BY pl.rowid ASC LIMIT ?2",
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
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for (rowid, person_id, item_id) in &rows {
            high_water = high_water.max(*rowid);
            observe_on(
                &tx,
                &Node::new("person", person_id),
                "apparait_dans",
                &Node::new("item", item_id),
                now(),
                "conversation",
            )?;
            count += 1;
        }
        tx.commit()?;
        Ok(())
    })?;
    set_cursor(db, "graph.person_links", high_water)?;
    Ok(count)
}

/// Un document confié dans une conversation rattachée à un projet appartient à
/// ce projet : c'est un fait, pas une inférence.
fn build_project_documents(db: &Db, budget: usize) -> Result<usize> {
    let from_cursor = cursor(db, "graph.session_documents");
    let rows: Vec<(i64, String, String, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT sd.added_at, sd.path, sd.name, s.project_id
             FROM session_documents sd
             JOIN sessions s ON s.id = sd.session_id
             WHERE sd.added_at > ?1 AND s.project_id IS NOT NULL
             ORDER BY sd.added_at ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![from_cursor, budget as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
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
    for (added_at, path, _name, project_id) in rows {
        high_water = high_water.max(added_at);
        let item_id: Option<String> = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT id FROM items WHERE path=?1 AND status='active' LIMIT 1",
                    params![path],
                    |r| r.get::<_, String>(0),
                )
                .ok())
            })
            .unwrap_or(None);
        let Some(item_id) = item_id else { continue };
        observe(
            db,
            &Node::new("item", &item_id),
            "classe_dans",
            &Node::new("project", &project_id),
            added_at,
            "conversation",
        )?;
        count += 1;
    }
    set_cursor(db, "graph.session_documents", high_water)?;
    Ok(count)
}

/// Reconstruction complète : la toile est dérivée, donc jetable.
pub fn rebuild(db: &Db) -> Result<()> {
    db.with(|c| {
        c.execute("DELETE FROM relations", [])?;
        c.execute(
            "DELETE FROM memory_state WHERE key LIKE 'graph.%'",
            [],
        )?;
        Ok(())
    })
}

// ————— Lecture de la toile —————

fn label_for(db: &Db, kind: &str, id: &str) -> String {
    let query = match kind {
        "person" => "SELECT name FROM people WHERE id=?1",
        "item" => "SELECT COALESCE(title, source_ref) FROM items WHERE id=?1",
        "event" => "SELECT title FROM events WHERE id=?1",
        "project" => "SELECT name FROM projects WHERE id=?1",
        "contact" => {
            return db
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT COALESCE(NULLIF(display_name,''), address) FROM contacts WHERE address=?1",
                        params![id],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap_or_else(|_| id.to_string()))
                })
                .unwrap_or_else(|_| id.to_string())
        }
        "self" => return "Toi".into(),
        _ => return id.to_string(),
    };
    db.read(|c| {
        Ok(c.query_row(query, params![id], |r| r.get::<_, String>(0))
            .unwrap_or_else(|_| id.to_string()))
    })
    .unwrap_or_else(|_| id.to_string())
}

/// Voisins directs d'un nœud, dans les deux sens, les plus observés d'abord.
pub fn neighbors(db: &Db, node: &Node, limit: usize) -> Result<Vec<Value>> {
    let rows: Vec<(String, String, String, i64, i64, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT kind, dst_kind, dst_id, observations, last_seen, 'sortant' FROM relations
             WHERE src_kind=?1 AND src_id=?2
             UNION ALL
             SELECT kind, src_kind, src_id, observations, last_seen, 'entrant' FROM relations
             WHERE dst_kind=?1 AND dst_id=?2
             ORDER BY observations DESC, last_seen DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![node.kind, node.id, limit as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    Ok(rows
        .into_iter()
        .map(|(kind, other_kind, other_id, observations, last_seen, direction)| {
            json!({
                "relation": kind,
                "direction": direction,
                "kind": other_kind,
                "id": other_id,
                "label": label_for(db, &other_kind, &other_id),
                "observations": observations,
                "last_seen": last_seen,
            })
        })
        .collect())
}

/// Les gens avec qui l'utilisateur échange le plus, tous canaux confondus.
pub fn top_correspondents(db: &Db, limit: usize) -> Result<Vec<Value>> {
    let rows: Vec<(String, String, i64, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            // `relations.kind` est qualifié : sans cela, SQLite peut résoudre
            // `kind` sur l'alias de sortie (`src_kind AS kind`) et le filtre ne
            // correspondrait alors jamais.
            "SELECT noeud_kind, noeud_id, SUM(observations) AS total, MAX(last_seen) AS vu FROM (
               SELECT src_kind AS noeud_kind, src_id AS noeud_id, observations, last_seen
                 FROM relations WHERE relations.kind='ecrit_a' AND dst_kind='self'
               UNION ALL
               SELECT dst_kind AS noeud_kind, dst_id AS noeud_id, observations, last_seen
                 FROM relations WHERE relations.kind='ecrit_a' AND src_kind='self'
             ) GROUP BY noeud_kind, noeud_id ORDER BY total DESC, vu DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    Ok(rows
        .into_iter()
        .map(|(kind, id, total, last_seen)| {
            json!({
                "kind": kind,
                "id": id,
                "label": label_for(db, &kind, &id),
                "echanges": total,
                "last_seen": last_seen,
            })
        })
        .collect())
}

/// Nombre d'échanges observés avec une adresse (dans les deux sens).
pub fn exchange_count(db: &Db, address: &str) -> i64 {
    let address = address.to_lowercase();
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(SUM(observations),0) FROM relations
             WHERE relations.kind='ecrit_a'
               AND ((src_kind='contact' AND src_id=?1) OR (dst_kind='contact' AND dst_id=?1))",
            params![address],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0))
    })
    .unwrap_or(0)
}

/// Ce que Syn sait d'une personne : ses adresses, ses derniers échanges, les
/// gens qu'elle croise, les documents où elle apparaît.
pub fn person_snapshot(db: &Db, node: &Node, limit: usize) -> Result<Value> {
    let voisins = neighbors(db, node, limit * 3)?;
    let mut documents = vec![];
    let mut gens = vec![];
    let mut rendez_vous = vec![];
    for v in voisins {
        match v["kind"].as_str().unwrap_or("") {
            "item" if documents.len() < limit => documents.push(v),
            "person" | "contact" if gens.len() < limit => gens.push(v),
            "event" if rendez_vous.len() < limit => rendez_vous.push(v),
            _ => {}
        }
    }
    Ok(json!({
        "noeud": {"kind": node.kind, "id": node.id, "label": label_for(db, &node.kind, &node.id)},
        "documents_lies": documents,
        "gens_en_commun": gens,
        "rendez_vous": rendez_vous,
    }))
}

/// Retrouve le nœud désigné par un nom ou une adresse, puis rend ce que Syn a
/// observé autour de lui.
///
/// Trois chemins, du plus sûr au plus large : une adresse écrite telle quelle,
/// une personne du carnet, un correspondant croisé dans les en-têtes. Rien
/// n'est inventé : sans nœud, la réponse le dit — et le dit au modèle, pour
/// qu'il ne comble pas le vide.
pub fn lookup(db: &Db, name: &str) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(json!({"trouve": false, "note": "Aucun nom fourni."}));
    }
    let folded = crate::db::fold(name);

    let node = if name.contains('@') {
        Some(Node::new("contact", &name.to_lowercase()))
    } else {
        let person: Option<String> = db.read(|c| {
            Ok(c.query_row(
                "SELECT id FROM people WHERE syn_fold(name) LIKE '%'||?1||'%'
                 ORDER BY length(name) LIMIT 1",
                params![folded],
                |r| r.get::<_, String>(0),
            )
            .ok())
        })?;
        match person {
            Some(id) => Some(Node::new("person", &id)),
            None => db
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT address FROM contacts
                         WHERE syn_fold(COALESCE(display_name,'')) LIKE '%'||?1||'%'
                            OR address LIKE '%'||?1||'%'
                         ORDER BY observations DESC LIMIT 1",
                        params![folded],
                        |r| r.get::<_, String>(0),
                    )
                    .ok())
                })?
                .map(|address| Node::new("contact", &address)),
        }
    };

    let Some(node) = node else {
        return Ok(json!({
            "trouve": false,
            "note": format!("Aucun lien observé autour de « {name} ». Ne suppose rien à son sujet.")
        }));
    };
    let mut snapshot = person_snapshot(db, &node, 5)?;
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("trouve".into(), json!(true));
        if node.kind == "contact" {
            object.insert(
                "echanges_observes".into(),
                json!(exchange_count(db, &node.id)),
            );
        }
    }
    Ok(snapshot)
}

pub fn stats(db: &Db) -> Result<Value> {
    db.read(|c| {
        let relations: i64 = c.query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0))?;
        let contacts: i64 = c.query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0))?;
        let noeuds: i64 = c.query_row(
            "SELECT COUNT(*) FROM (
               SELECT src_kind || src_id AS n FROM relations
               UNION SELECT dst_kind || dst_id FROM relations)",
            [],
            |r| r.get(0),
        )?;
        let par_type = {
            let mut stmt = c.prepare(
                "SELECT kind, COUNT(*) FROM relations GROUP BY kind ORDER BY COUNT(*) DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({"relation": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            out
        };
        Ok(json!({
            "relations": relations,
            "noeuds": noeuds,
            "contacts": contacts,
            "par_type": par_type,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Db {
        let dir = std::env::temp_dir().join(format!("syn-graph-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db"), &"a".repeat(64)).unwrap()
    }

    fn ajoute_mail(db: &Db, id: &str, body: &str, created_at: i64, ingested_at: i64) {
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, created_at, ingested_at, status)
                 VALUES (?1,'mail',?1,'email','Objet',?2,?3,?4,'active')",
                params![id, body, created_at, ingested_at],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn les_entetes_donnent_expediteur_et_destinataires() {
        let (from, to) = parse_headers(
            "De : Julie Martin <julie@exemple.fr>\nÀ : paul@moi.fr, Marc <marc@exemple.fr>\nObjet : Devis\n\nBonjour",
        );
        assert_eq!(from.len(), 1);
        assert_eq!(from[0].1, "julie@exemple.fr");
        assert_eq!(to.len(), 2);
        assert_eq!(to[1].0, "Marc");
    }

    #[test]
    fn le_corps_du_mail_nest_jamais_lu_comme_un_entete() {
        let (from, _) = parse_headers(
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : x\n\nDe : faux@pirate.fr",
        );
        assert_eq!(from.len(), 1, "seules les 4 premières lignes font foi");
    }

    /// Une boîte réelle : l'utilisateur figure dans tous les messages, ses
    /// correspondants dans une partie seulement.
    fn boite_realiste(db: &Db) {
        let correspondants = ["julie@exemple.fr", "marc@exemple.fr", "edf@edf.fr"];
        let mut horodatage = 1_700_000_000;
        for (index, correspondant) in correspondants.iter().enumerate() {
            for tour in 0..2 {
                horodatage += 1;
                ajoute_mail(
                    db,
                    &format!("recu{index}-{tour}"),
                    &format!("De : Contact <{correspondant}>\nÀ : paul@moi.fr\nObjet : x\n\n"),
                    horodatage,
                    horodatage,
                );
            }
        }
        for tour in 0..2 {
            horodatage += 1;
            ajoute_mail(
                db,
                &format!("envoye{tour}"),
                "De : Paul <paul@moi.fr>\nÀ : julie@exemple.fr\nObjet : x\n\n",
                horodatage,
                horodatage,
            );
        }
    }

    #[test]
    fn syn_deduit_ladresse_de_lutilisateur() {
        let db = base();
        boite_realiste(&db);
        build(&db, 100).unwrap();
        assert_eq!(self_addresses(&db), vec!["paul@moi.fr".to_string()]);
    }

    /// Deux adresses aussi présentes l'une que l'autre : Syn se tait plutôt que
    /// de prendre son correspondant pour lui-même — et laisse l'utilisateur
    /// trancher.
    #[test]
    fn sans_majorite_nette_syn_ne_devine_pas() {
        let db = base();
        for i in 0..6 {
            ajoute_mail(
                &db,
                &format!("m{i}"),
                "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : x\n\n",
                1_700_000_000 + i,
                1_700_000_000 + i,
            );
        }
        build(&db, 100).unwrap();
        assert!(self_addresses(&db).is_empty());
        let candidats = list_identity_candidates(&db).unwrap();
        assert_eq!(candidats.len(), 2);

        set_self_address(&db, "paul@moi.fr", true).unwrap();
        assert_eq!(self_addresses(&db), vec!["paul@moi.fr".to_string()]);
    }

    #[test]
    fn la_toile_relie_lutilisateur_a_ses_correspondants() {
        let db = base();
        boite_realiste(&db);
        build(&db, 100).unwrap();
        let top = top_correspondents(&db, 5).unwrap();
        assert_eq!(top[0]["id"], "julie@exemple.fr");
        assert_eq!(top[0]["echanges"], 4, "2 reçus + 2 envoyés");
        assert_eq!(exchange_count(&db, "edf@edf.fr"), 2);
    }

    /// Une seconde passe ne doit pas recompter les mêmes mails : sinon toute
    /// fréquence observée devient un artefact du nombre de passes.
    #[test]
    fn une_seconde_passe_ne_recompte_pas() {
        let db = base();
        ajoute_mail(
            &db,
            "m1",
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : x\n\n",
            1_700_000_000,
            1_700_000_000,
        );
        build(&db, 100).unwrap();
        let avant = exchange_count(&db, "julie@exemple.fr");
        build(&db, 100).unwrap();
        assert_eq!(exchange_count(&db, "julie@exemple.fr"), avant);
    }

    #[test]
    fn une_liste_de_diffusion_ne_cree_pas_de_liens_entre_inconnus() {
        let db = base();
        let destinataires = (0..8)
            .map(|i| format!("p{i}@liste.fr"))
            .collect::<Vec<_>>()
            .join(", ");
        ajoute_mail(
            &db,
            "m1",
            &format!("De : Info <info@liste.fr>\nÀ : {destinataires}\nObjet : x\n\n"),
            1_700_000_000,
            1_700_000_000,
        );
        build(&db, 100).unwrap();
        let liens: i64 = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM relations WHERE kind='co_destinataire'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(liens, 0);
    }
}
