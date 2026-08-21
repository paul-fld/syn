//! Les réflexes de Syn : ce qu'il remarque sans qu'on le lui demande.
//!
//! L'arbitre (budget de rareté, fenêtres calmes, anti-répétition, mode travail)
//! existait déjà et était solide ; il ne surveillait que trois choses — disque
//! plein, processeur chaud, batterie faible. Tout ce qui touchait à la VIE de
//! l'utilisateur, et non à sa machine, était absent.
//!
//! Chaque réflexe ci-dessous :
//! * est **déterministe** — aucun modèle n'est appelé, donc rien n'est inventé ;
//! * est **explicable** — sa raison dit ce que Syn a vu (invariant du doc
//!   Proactivité : un surfaçage sans raison affichée est un bug) ;
//! * est **débrayable** — il a sa ligne dans `triggers`, visible et coupable
//!   depuis « Mes programmations » ;
//! * se **tait quand il ne sait pas** — sans adresse de l'utilisateur connue,
//!   par exemple, le suivi des messages sans réponse ne s'invente rien.

use crate::bus::Bus;
use crate::db::{now, Db};
use crate::error::Result;
use crate::memory::graph;
use rusqlite::params;
use serde_json::Value;

use super::{arbitrate, Candidate};

/// Un réflexe : son identifiant stable, sa description pour l'interface, et la
/// fréquence à laquelle il vaut la peine d'être évalué.
struct Reflexe {
    id: &'static str,
    condition: &'static str,
    libelle: &'static str,
    priorite: &'static str,
    intervalle: i64,
}

const REFLEXES: &[Reflexe] = &[
    Reflexe {
        id: "sys.mail_sans_reponse",
        condition: "mail.sans_reponse",
        libelle: "Message resté sans réponse",
        priorite: "important",
        intervalle: 2 * 3600,
    },
    Reflexe {
        id: "sys.preparation_reunion",
        condition: "agenda.reunion_imminente",
        libelle: "Réunion imminente, avec de quoi la préparer",
        priorite: "important",
        intervalle: 240,
    },
    Reflexe {
        id: "sys.engagement_oublie",
        condition: "engagement.sans_suite",
        libelle: "Engagement pris et resté sans suite",
        priorite: "important",
        intervalle: 6 * 3600,
    },
    Reflexe {
        id: "sys.dossier_qui_deborde",
        condition: "fichiers.dossier_encombre",
        libelle: "Dossier qui déborde, à ranger",
        priorite: "info",
        intervalle: 12 * 3600,
    },
    Reflexe {
        id: "sys.anniversaire_proche",
        condition: "personne.anniversaire",
        libelle: "Anniversaire d'un proche dans quelques jours",
        priorite: "info",
        intervalle: 12 * 3600,
    },
];

/// Inscrit les réflexes manquants. Ils deviennent ainsi visibles et coupables
/// dans « Mes programmations » : rien de ce que Syn surveille ne doit rester
/// caché à l'utilisateur.
pub fn ensure_registered(db: &Db) -> Result<()> {
    db.with(|c| {
        for reflexe in REFLEXES {
            c.execute(
                "INSERT INTO triggers (id, type, condition, priority, reason_template, action, source, enabled)
                 VALUES (?1, 'context', ?2, ?3, ?4, 'notify', 'system', 1)
                 ON CONFLICT(id) DO UPDATE SET
                   condition = excluded.condition,
                   reason_template = excluded.reason_template,
                   priority = excluded.priority",
                params![
                    reflexe.id,
                    reflexe.condition,
                    reflexe.priorite,
                    reflexe.libelle
                ],
            )?;
        }
        Ok(())
    })
}

fn enabled(db: &Db, id: &str) -> bool {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT enabled FROM triggers WHERE id=?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(1)
            != 0)
    })
    .unwrap_or(true)
}

/// Évite de repayer le coût d'un réflexe à chaque minute : chacun a son rythme.
fn due(db: &Db, id: &str, intervalle: i64) -> bool {
    let key = format!("reflexe.{id}");
    let last = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT value FROM memory_state WHERE key=?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0))
        })
        .unwrap_or(0);
    if now() - last < intervalle {
        return false;
    }
    let _ = db.with(|c| {
        c.execute(
            "INSERT INTO memory_state (key, value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("reflexe.{id}"), now().to_string()],
        )?;
        Ok(())
    });
    true
}

fn note_fired(db: &Db, id: &str) {
    let _ = db.with(|c| {
        c.execute(
            "UPDATE triggers SET last_fired=?2 WHERE id=?1",
            params![id, now()],
        )?;
        Ok(())
    });
}

/// Passe complète. Appelée par la boucle de proactivité, après les seuils système.
pub fn evaluate(db: &Db, bus: &Bus) -> Result<()> {
    ensure_registered(db)?;
    let settings = crate::settings::load(db).unwrap_or_default();
    let speak = crate::i18n::ambient_speak(db, &settings);
    for reflexe in REFLEXES {
        if !enabled(db, reflexe.id) || !due(db, reflexe.id, reflexe.intervalle) {
            continue;
        }
        let candidats = match reflexe.condition {
            "mail.sans_reponse" => mails_sans_reponse(db, speak)?,
            "agenda.reunion_imminente" => reunions_a_preparer(db, speak)?,
            "engagement.sans_suite" => engagements_sans_suite(db, speak)?,
            "fichiers.dossier_encombre" => dossiers_qui_debordent(db, speak)?,
            "personne.anniversaire" => anniversaires_proches(db, speak)?,
            _ => vec![],
        };
        for (reason, body) in candidats {
            let surfaced = arbitrate(
                db,
                bus,
                Candidate {
                    trigger_id: Some(reflexe.id.to_string()),
                    kind: "reflexe".into(),
                    reason,
                    body,
                    priority: reflexe.priorite.into(),
                },
            )?;
            if surfaced {
                note_fired(db, reflexe.id);
            }
        }
    }
    Ok(())
}

fn jours(depuis: i64) -> i64 {
    ((now() - depuis) / 86_400).max(0)
}

fn date_courte(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%e %B").to_string())
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ————— 1. Messages restés sans réponse —————

/// Un message attend une réponse quand il vient de quelqu'un que l'utilisateur
/// connaît, qu'il lui est adressé directement (et non à une liste), et qu'aucun
/// message ne lui a été envoyé depuis.
///
/// Sans adresse connue de l'utilisateur, « reçu » et « envoyé » sont
/// indiscernables : le réflexe se tait plutôt que d'alerter à contresens.
pub struct SansReponse {
    pub objet: String,
    pub qui: String,
    pub jours: i64,
    pub source_ref: String,
}

/// Les messages en attente, au plus `limite`. Sert au réflexe (qui notifie) et
/// au brief du matin (qui les résume) : une seule définition, deux surfaces.
pub fn en_attente_de_reponse(db: &Db, limite: usize) -> Result<Vec<SansReponse>> {
    let moi = graph::self_addresses(db);
    if moi.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<(String, String, i64, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT COALESCE(title,'(sans objet)'), substr(COALESCE(body,''),1,600), created_at, source_ref
             FROM items
             WHERE source='mail' AND status='active' AND created_at IS NOT NULL
               AND created_at BETWEEN ?1 AND ?2
             ORDER BY created_at DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(params![now() - 30 * 86_400, now() - 3 * 86_400], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;

    let mut candidats = vec![];
    let mut deja_vus: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut examines = 0;
    for (objet, body, recu_le, source_ref) in rows {
        // Borne de travail : au-delà, on a de toute façon assez de matière, et
        // ce réflexe ne doit jamais devenir un balayage coûteux.
        examines += 1;
        if examines > 80 {
            break;
        }
        let (expediteurs, destinataires) = graph::parse_headers(&body);
        let Some((nom, adresse)) = expediteurs.first().cloned() else {
            continue;
        };
        if moi.iter().any(|m| *m == adresse) {
            continue; // c'est un message de l'utilisateur lui-même
        }
        // Une seule relance par personne : trois messages de la même personne
        // sans réponse, c'est une seule chose à faire, pas trois notifications.
        if !deja_vus.insert(adresse.clone()) {
            continue;
        }
        // Adressé à lui, pas à une liste de diffusion.
        if destinataires.len() > 3 || !destinataires.iter().any(|(_, a)| moi.contains(a)) {
            continue;
        }
        // Quelqu'un dont il a l'habitude : un inconnu qui écrit une fois n'est
        // pas une dette de réponse.
        if graph::exchange_count(db, &adresse) < 3 && !est_une_personne_connue(db, &adresse) {
            continue;
        }
        if a_ecrit_depuis(db, &adresse, recu_le, &moi)? {
            continue;
        }
        candidats.push(SansReponse {
            objet,
            qui: if nom.is_empty() { adresse } else { nom },
            jours: jours(recu_le),
            source_ref,
        });
        if candidats.len() == limite {
            break;
        }
    }
    Ok(candidats)
}

fn mails_sans_reponse(db: &Db, speak: crate::i18n::Speak) -> Result<Vec<(String, String)>> {
    Ok(en_attente_de_reponse(db, 2)?
        .into_iter()
        .map(|attente| {
            let corps = if speak.is_en() {
                format!(
                    "« {} » from {}, received {} days ago — nothing sent back since.",
                    attente.objet, attente.qui, attente.jours
                )
            } else {
                format!(
                    "« {} » de {}, reçu il y a {} jours — {} n'as rien envoyé depuis.",
                    attente.objet,
                    attente.qui,
                    attente.jours,
                    speak.pick("tu", "vous", "you")
                )
            };
            (
                speak
                    .either("Message resté sans réponse", "Message still unanswered")
                    .to_string(),
                corps,
            )
        })
        .collect())
}

/// Le réflexe est-il actif ? Le brief du matin s'aligne sur le même
/// interrupteur : couper le réflexe coupe aussi sa ligne dans le brief.
pub fn est_actif(db: &Db, id: &str) -> bool {
    enabled(db, id)
}

fn est_une_personne_connue(db: &Db, adresse: &str) -> bool {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM people WHERE comm_channels LIKE '%'||lower(?1)||'%' LIMIT 1",
            params![adresse],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })
    .unwrap_or(false)
}

/// L'utilisateur a-t-il écrit à cette adresse depuis cette date ?
fn a_ecrit_depuis(db: &Db, adresse: &str, depuis: i64, moi: &[String]) -> Result<bool> {
    let rows: Vec<String> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT substr(COALESCE(body,''),1,600) FROM items
             WHERE source='mail' AND status='active' AND created_at > ?1
               AND instr(lower(substr(COALESCE(body,''),1,600)), ?2) > 0
             ORDER BY created_at ASC LIMIT 40",
        )?;
        let rows = stmt.query_map(params![depuis, adresse.to_lowercase()], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    for body in rows {
        let (expediteurs, destinataires) = graph::parse_headers(&body);
        let de_moi = expediteurs.iter().any(|(_, a)| moi.contains(a));
        let vers_lui = destinataires.iter().any(|(_, a)| a == adresse);
        if de_moi && vers_lui {
            return Ok(true);
        }
    }
    Ok(false)
}

// ————— 2. Réunions à préparer —————

/// Une réunion qui approche avec des invités : Syn ressort ce que l'utilisateur
/// a échangé avec eux, pour qu'il n'arrive pas les mains vides.
fn reunions_a_preparer(db: &Db, speak: crate::i18n::Speak) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT title, COALESCE(attendees,'[]'), \"start\" FROM events
             WHERE \"start\" BETWEEN ?1 AND ?2 AND attendees IS NOT NULL AND attendees <> '[]'",
        )?;
        let rows = stmt.query_map(params![now() + 300, now() + 3600], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;

    let moi = graph::self_addresses(db);
    let mut candidats = vec![];
    for (titre, attendees, start) in rows {
        let minutes = ((start - now()) / 60).max(0);
        let adresses: Vec<String> = serde_json::from_str::<Value>(&attendees)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| {
                entry["email"]
                    .as_str()
                    .or_else(|| entry.as_str())
                    .map(|a| a.trim().to_lowercase())
            })
            .filter(|a| a.contains('@') && !moi.contains(a))
            .collect();

        let mut noms = vec![];
        let mut echanges = vec![];
        for adresse in adresses.iter().take(3) {
            noms.push(nom_affiche(db, adresse));
            for (objet, at) in derniers_echanges(db, adresse, 2)? {
                echanges.push(format!("« {objet} » ({})", date_courte(at)));
            }
        }
        let avec = if noms.is_empty() {
            String::new()
        } else {
            format!(" {} {}", speak.either("avec", "with"), noms.join(", "))
        };
        let rappel = if echanges.is_empty() {
            speak
                .either(
                    "Aucun échange récent retrouvé avec les participants.",
                    "No recent exchange found with the attendees.",
                )
                .to_string()
        } else {
            echanges.truncate(3);
            format!(
                "{} {}.",
                speak.pick(
                    "Vos derniers échanges :",
                    "Vos derniers échanges :",
                    "Your latest exchanges:"
                ),
                echanges.join(", ")
            )
        };
        candidats.push((
            speak
                .either("Réunion imminente", "Meeting starting soon")
                .to_string(),
            if speak.is_en() {
                format!("« {titre} »{avec} in {minutes} min. {rappel}")
            } else {
                format!("« {titre} »{avec} dans {minutes} min. {rappel}")
            },
        ));
    }
    Ok(candidats)
}

fn nom_affiche(db: &Db, adresse: &str) -> String {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(NULLIF(display_name,''), address) FROM contacts WHERE address=?1",
            params![adresse],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|_| adresse.to_string()))
    })
    .unwrap_or_else(|_| adresse.to_string())
}

fn derniers_echanges(db: &Db, adresse: &str, limit: usize) -> Result<Vec<(String, i64)>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT COALESCE(title,'(sans objet)'), COALESCE(created_at, ingested_at) FROM items
             WHERE source='mail' AND status='active'
               AND instr(lower(substr(COALESCE(body,''),1,600)), ?1) > 0
             ORDER BY COALESCE(created_at, ingested_at) DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![adresse.to_lowercase(), limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

// ————— 3. Engagements sans suite —————

/// Un engagement pris envers quelqu'un, sans échéance et sans mouvement depuis
/// une semaine : c'est exactement ce qu'un assistant doit rappeler.
fn engagements_sans_suite(db: &Db, speak: crate::i18n::Speak) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, Option<String>, i64)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT c.text, p.name, COALESCE(i.created_at, i.ingested_at)
             FROM commitments c
             LEFT JOIN people p ON p.id = c.person_id
             JOIN items i ON i.source_ref = c.source_ref
             WHERE c.status='open' AND c.due IS NULL
               AND COALESCE(i.created_at, i.ingested_at) < ?1
             ORDER BY COALESCE(i.created_at, i.ingested_at) ASC LIMIT 2",
        )?;
        let rows = stmt.query_map(params![now() - 7 * 86_400], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    Ok(rows
        .into_iter()
        .map(|(texte, personne, pris_le)| {
            let corps = if speak.is_en() {
                let envers = personne
                    .map(|nom| format!(" to {nom}"))
                    .unwrap_or_default();
                format!(
                    "You committed{envers} to « {texte} » {} days ago, with no follow-up since.",
                    jours(pris_le)
                )
            } else {
                let envers = personne
                    .map(|nom| format!(" envers {nom}"))
                    .unwrap_or_default();
                format!(
                    "{} t'étais engagé{envers} à « {texte} » il y a {} jours, sans suite depuis.",
                    speak.pick("Tu", "Vous vous", "You"),
                    jours(pris_le)
                )
            };
            (
                speak
                    .either("Engagement sans suite", "Commitment with no follow-up")
                    .to_string(),
                corps,
            )
        })
        .collect())
}

// ————— 4. Dossiers qui débordent —————

/// Un dossier qui accumule sans être rangé. Syn sait le ranger (files.reorganize) :
/// autant le proposer au moment où ça déborde, pas trois mois plus tard.
fn dossiers_qui_debordent(db: &Db, speak: crate::i18n::Speak) -> Result<Vec<(String, String)>> {
    let chemins: Vec<String> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT path FROM items
             WHERE source='files' AND status='active' AND path IS NOT NULL
               AND COALESCE(mtime, created_at, ingested_at) >= ?1
             ORDER BY COALESCE(mtime, created_at, ingested_at) DESC LIMIT 3000",
        )?;
        let rows = stmt.query_map(params![now() - 30 * 86_400], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;

    let mut par_dossier: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for chemin in chemins {
        let Some(parent) = std::path::Path::new(&chemin).parent() else {
            continue;
        };
        *par_dossier
            .entry(parent.to_string_lossy().to_string())
            .or_insert(0) += 1;
    }
    let mut classement: Vec<(String, usize)> = par_dossier.into_iter().collect();
    classement.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(classement
        .into_iter()
        .filter(|(_, count)| *count >= 40)
        .take(1)
        .map(|(dossier, count)| {
            let nom = std::path::Path::new(&dossier)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| dossier.clone());
            (
                speak
                    .either("Dossier qui déborde", "Folder filling up")
                    .to_string(),
                if speak.is_en() {
                    format!(
                        "{count} files landed in « {nom} » this month. Ask me to tidy it up if you want."
                    )
                } else {
                    format!(
                        "{count} fichiers sont arrivés dans « {nom} » ce mois-ci. {} de le ranger si {} veux.",
                        speak.pick("Demande-moi", "Demandez-moi", "Ask me"),
                        speak.pick("tu", "vous", "you")
                    )
                },
            )
        })
        .collect())
}

// ————— 5. Anniversaires —————

/// Le jour même, c'est déjà trop tard pour un cadeau : Syn prévient avant.
fn anniversaires_proches(db: &Db, speak: crate::i18n::Speak) -> Result<Vec<(String, String)>> {
    let mut candidats = vec![];
    for dans in [2i64, 7] {
        let cible = (chrono::Local::now() + chrono::Duration::days(dans))
            .format("%m-%d")
            .to_string();
        let noms: Vec<String> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM people
                 WHERE birthday IS NOT NULL AND (birthday = ?1 OR substr(birthday, 6) = ?1) LIMIT 3",
            )?;
            let rows = stmt.query_map(params![cible], |r| r.get::<_, String>(0))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for nom in noms {
            candidats.push((
                speak
                    .either("Anniversaire à venir", "Birthday coming up")
                    .to_string(),
                if speak.is_en() {
                    format!("It's {nom}'s birthday in {dans} days.")
                } else {
                    format!("C'est l'anniversaire de {nom} dans {dans} jours.")
                },
            ));
        }
    }
    Ok(candidats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::new_id;

    fn base() -> Db {
        let dir = std::env::temp_dir().join(format!("syn-reflexes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db"), &"d".repeat(64)).unwrap()
    }

    fn mail(db: &Db, body: &str, objet: &str, at: i64) {
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES (?1,'mail',?1,'email',?2,?3,?4,?4,'active')",
                params![new_id(), objet, body, at],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn moi_cest_paul(db: &Db) {
        graph::set_self_address(db, "paul@moi.fr", true).unwrap();
    }

    fn correspondant_habituel(db: &Db, adresse: &str) {
        for _ in 0..3 {
            graph::observe(
                db,
                &graph::Node::new("contact", adresse),
                "ecrit_a",
                &graph::Node::moi(),
                now(),
                "mail",
            )
            .unwrap();
        }
    }

    #[test]
    fn un_mail_de_proche_reste_sans_reponse_est_signale() {
        let db = base();
        moi_cest_paul(&db);
        correspondant_habituel(&db, "julie@exemple.fr");
        mail(
            &db,
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\nTu en penses quoi ?",
            "Devis toiture",
            now() - 5 * 86_400,
        );
        let candidats = mails_sans_reponse(&db, crate::i18n::Speak::fr(false)).unwrap();
        assert_eq!(candidats.len(), 1);
        assert!(candidats[0].1.contains("Devis toiture"));
        assert!(candidats[0].1.contains("5 jours"));
    }

    /// Trois messages de la même personne, c'est une seule chose à faire.
    #[test]
    fn une_seule_relance_par_personne() {
        let db = base();
        moi_cest_paul(&db);
        correspondant_habituel(&db, "julie@exemple.fr");
        for jour in 4..7 {
            mail(
                &db,
                "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\n",
                &format!("Relance {jour}"),
                now() - jour * 86_400,
            );
        }
        assert_eq!(en_attente_de_reponse(&db, 3).unwrap().len(), 1);
    }

    #[test]
    fn un_mail_auquel_on_a_repondu_nest_pas_signale() {
        let db = base();
        moi_cest_paul(&db);
        correspondant_habituel(&db, "julie@exemple.fr");
        mail(
            &db,
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\n",
            "Devis toiture",
            now() - 5 * 86_400,
        );
        mail(
            &db,
            "De : Paul <paul@moi.fr>\nÀ : julie@exemple.fr\nObjet : Re: Devis\n\n",
            "Re: Devis toiture",
            now() - 4 * 86_400,
        );
        assert!(mails_sans_reponse(&db, crate::i18n::Speak::fr(false)).unwrap().is_empty());
    }

    /// Une newsletter n'est pas une dette de réponse.
    #[test]
    fn un_inconnu_ou_une_liste_ne_declenche_rien() {
        let db = base();
        moi_cest_paul(&db);
        mail(
            &db,
            "De : Promo <promo@boutique.fr>\nÀ : paul@moi.fr\nObjet : -30 %\n\n",
            "Soldes",
            now() - 5 * 86_400,
        );
        correspondant_habituel(&db, "info@asso.fr");
        mail(
            &db,
            "De : Info <info@asso.fr>\nÀ : paul@moi.fr, a@x.fr, b@x.fr, c@x.fr\nObjet : Réunion\n\n",
            "Lettre de l'asso",
            now() - 5 * 86_400,
        );
        assert!(mails_sans_reponse(&db, crate::i18n::Speak::fr(false)).unwrap().is_empty());
    }

    /// Sans adresse connue de l'utilisateur, Syn ne peut pas distinguer un
    /// message reçu d'un message envoyé : il doit se taire.
    #[test]
    fn sans_identite_connue_le_reflexe_se_tait() {
        let db = base();
        correspondant_habituel(&db, "julie@exemple.fr");
        mail(
            &db,
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\n",
            "Devis toiture",
            now() - 5 * 86_400,
        );
        assert!(mails_sans_reponse(&db, crate::i18n::Speak::fr(false)).unwrap().is_empty());
    }

    #[test]
    fn une_reunion_imminente_ressort_les_derniers_echanges() {
        let db = base();
        moi_cest_paul(&db);
        db.with(|c| {
            c.execute(
                "INSERT INTO contacts (address, display_name, observations, first_seen, last_seen)
                 VALUES ('julie@exemple.fr','Julie Martin',3,1,1)",
                [],
            )?;
            c.execute(
                "INSERT INTO events (id,source,source_ref,title,\"start\",attendees)
                 VALUES ('e1','apple','r1','Point chantier',?1,'[{\"email\":\"julie@exemple.fr\"}]')",
                params![now() + 1800],
            )?;
            Ok(())
        })
        .unwrap();
        mail(
            &db,
            "De : Julie <julie@exemple.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\n",
            "Devis toiture",
            now() - 2 * 86_400,
        );
        let candidats = reunions_a_preparer(&db, crate::i18n::Speak::fr(false)).unwrap();
        assert_eq!(candidats.len(), 1);
        assert!(candidats[0].1.contains("Julie Martin"));
        assert!(candidats[0].1.contains("Devis toiture"));
    }

    #[test]
    fn les_reflexes_sont_inscrits_et_debrayables() {
        let db = base();
        ensure_registered(&db).unwrap();
        let count: i64 = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM triggers WHERE source='system'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(count as usize, REFLEXES.len());
        assert!(enabled(&db, "sys.mail_sans_reponse"));
        db.with(|c| {
            c.execute(
                "UPDATE triggers SET enabled=0 WHERE id='sys.mail_sans_reponse'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(!enabled(&db, "sys.mail_sans_reponse"));
        // Une seconde inscription ne réactive pas ce que l'utilisateur a coupé.
        ensure_registered(&db).unwrap();
        assert!(!enabled(&db, "sys.mail_sans_reponse"));
    }

    #[test]
    fn un_reflexe_nest_pas_reevalue_a_chaque_minute() {
        let db = base();
        assert!(due(&db, "sys.test", 3600));
        assert!(!due(&db, "sys.test", 3600));
    }
}
