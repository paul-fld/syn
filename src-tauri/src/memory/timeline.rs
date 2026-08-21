//! La ligne de temps : ce qui s'est passé, et quand.
//!
//! La matière existait déjà — mails, documents, rendez-vous, engagements,
//! actions exécutées, conversations — mais chaque table n'était interrogeable
//! que par elle-même et par ressemblance. « Que s'est-il passé la semaine
//! dernière ? » ou « reprends là où on en était » n'avaient donc aucune requête
//! derrière eux.
//!
//! Ce module ne duplique rien : il LIT les tables existantes par leur date. La
//! ligne de temps est donc toujours juste, sans risque de dérive entre deux
//! copies de la même vérité.

use crate::db::Db;
use crate::error::Result;
use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub at: i64,
    /// mail_recu | mail_envoye | document | rendez_vous | tache | engagement | action | conversation
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    pub source_ref: Option<String>,
}

/// Fenêtre demandée. Les bornes sont inclusives ; `None` = pas de borne.
#[derive(Debug, Clone, Default)]
pub struct Window {
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Restreint aux entrées citant ce texte (personne, projet, sujet).
    pub about: Option<String>,
    pub kinds: Option<Vec<String>>,
    pub limit: usize,
}

impl Window {
    pub fn last_days(days: i64, limit: usize) -> Self {
        Window {
            from: Some(crate::db::now() - days * 86_400),
            to: None,
            about: None,
            kinds: None,
            limit,
        }
    }
    fn keeps(&self, kind: &str) -> bool {
        match &self.kinds {
            None => true,
            Some(kinds) => kinds.iter().any(|k| k == kind),
        }
    }
    fn from_ts(&self) -> i64 {
        self.from.unwrap_or(0)
    }
    fn to_ts(&self) -> i64 {
        self.to.unwrap_or(i64::MAX)
    }
}

/// Construit la chronologie. Chaque source est bornée séparément puis
/// fusionnée : une boîte mail bavarde ne peut pas éclipser tout le reste.
pub fn build(db: &Db, window: &Window) -> Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = vec![];
    let per_source = window.limit.max(5);
    let (from, to) = (window.from_ts(), window.to_ts());
    let filtre = window
        .about
        .as_deref()
        .map(crate::db::fold)
        .unwrap_or_default();
    let moi = crate::memory::graph::self_addresses(db);

    // — Mails (le sens de l'échange vient des adresses de l'utilisateur) —
    if window.keeps("mail_recu") || window.keeps("mail_envoye") {
        let rows: Vec<(String, String, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT COALESCE(title,'(sans objet)'), substr(COALESCE(body,''),1,400), source_ref,
                        COALESCE(created_at, ingested_at)
                 FROM items
                 WHERE source='mail' AND status='active'
                   AND COALESCE(created_at, ingested_at) BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(COALESCE(title,'') || ' ' || substr(COALESCE(body,''),1,400)) LIKE '%'||?3||'%')
                 ORDER BY COALESCE(created_at, ingested_at) DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (title, body, source_ref, at) in rows {
            let (expediteurs, destinataires) = crate::memory::graph::parse_headers(&body);
            let envoye_par_moi = expediteurs
                .iter()
                .any(|(_, address)| moi.iter().any(|m| m == address));
            let kind = if envoye_par_moi {
                "mail_envoye"
            } else {
                "mail_recu"
            };
            if !window.keeps(kind) {
                continue;
            }
            let interlocuteur = if envoye_par_moi {
                destinataires.first()
            } else {
                expediteurs.first()
            }
            .map(|(name, address)| {
                if name.is_empty() {
                    address.clone()
                } else {
                    name.clone()
                }
            });
            entries.push(Entry {
                at,
                kind: kind.into(),
                title,
                detail: interlocuteur.map(|qui| {
                    if envoye_par_moi {
                        format!("à {qui}")
                    } else {
                        format!("de {qui}")
                    }
                }),
                source_ref: Some(source_ref),
            });
        }
    }

    // — Documents touchés —
    if window.keeps("document") {
        let rows: Vec<(Option<String>, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT title, source_ref, COALESCE(mtime, created_at, ingested_at) FROM items
                 WHERE source IN ('files','cloud') AND status='active'
                   AND COALESCE(mtime, created_at, ingested_at) BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(COALESCE(title,'') || ' ' || COALESCE(path,'')) LIKE '%'||?3||'%')
                 ORDER BY COALESCE(mtime, created_at, ingested_at) DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (title, source_ref, at) in rows {
            entries.push(Entry {
                at,
                kind: "document".into(),
                title: title.unwrap_or_else(|| source_ref.clone()),
                detail: Some("document modifié".into()),
                source_ref: Some(source_ref),
            });
        }
    }

    // — Rendez-vous —
    if window.keeps("rendez_vous") {
        let rows: Vec<(String, Option<String>, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT title, location, id, \"start\" FROM events
                 WHERE \"start\" BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(title || ' ' || COALESCE(location,'') || ' ' || COALESCE(attendees,'')) LIKE '%'||?3||'%')
                 ORDER BY \"start\" DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (title, location, id, at) in rows {
            entries.push(Entry {
                at,
                kind: "rendez_vous".into(),
                title,
                detail: location,
                source_ref: Some(id),
            });
        }
    }

    // — Engagements —
    if window.keeps("engagement") {
        let rows: Vec<(String, Option<String>, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT c.text, p.name, c.id, c.due FROM commitments c
                 LEFT JOIN people p ON p.id = c.person_id
                 WHERE c.due IS NOT NULL AND c.due BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(c.text || ' ' || COALESCE(p.name,'')) LIKE '%'||?3||'%')
                 ORDER BY c.due DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (text, person, id, at) in rows {
            entries.push(Entry {
                at,
                kind: "engagement".into(),
                title: text,
                detail: person.map(|name| format!("avec {name}")),
                source_ref: Some(id),
            });
        }
    }

    // — Ce que Syn a fait pour l'utilisateur —
    if window.keeps("action") {
        let rows: Vec<(String, Option<String>, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT tool, preview, id, created_at FROM actions_log
                 WHERE status='executed' AND created_at BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(COALESCE(preview,'') || ' ' || tool) LIKE '%'||?3||'%')
                 ORDER BY created_at DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (tool, preview, id, at) in rows {
            entries.push(Entry {
                at,
                kind: "action".into(),
                title: preview.unwrap_or_else(|| tool.clone()),
                detail: Some("fait par Syn".into()),
                source_ref: Some(id),
            });
        }
    }

    // — Conversations —
    if window.keeps("conversation") {
        let rows: Vec<(Option<String>, String, i64)> = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT title, id, created_at FROM sessions
                 WHERE created_at BETWEEN ?1 AND ?2
                   AND (?3 = '' OR syn_fold(COALESCE(title,'')) LIKE '%'||?3||'%')
                 ORDER BY created_at DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![from, to, filtre, per_source as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (title, id, at) in rows {
            entries.push(Entry {
                at,
                kind: "conversation".into(),
                title: title.unwrap_or_else(|| "Conversation".into()),
                detail: Some("conversation avec Syn".into()),
                source_ref: Some(id),
            });
        }
    }

    entries.sort_by(|a, b| b.at.cmp(&a.at));
    entries.truncate(window.limit);
    Ok(entries)
}

fn jour(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%A %e %B %Y")
                .to_string()
        })
        .unwrap_or_default()
}

fn heure(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Hh%M").to_string())
        .unwrap_or_default()
}

/// Rendu groupé par jour — la forme que l'interface affiche et que le modèle lit.
pub fn grouped(db: &Db, window: &Window) -> Result<Value> {
    let entries = build(db, window)?;
    let mut jours: Vec<Value> = vec![];
    for entry in &entries {
        let libelle = jour(entry.at);
        let ligne = json!({
            "at": entry.at,
            "heure": heure(entry.at),
            "kind": entry.kind,
            "title": entry.title,
            "detail": entry.detail,
            "source_ref": entry.source_ref,
        });
        match jours.last_mut() {
            Some(dernier) if dernier["jour"] == libelle => {
                if let Some(list) = dernier["entrees"].as_array_mut() {
                    list.push(ligne);
                }
            }
            _ => jours.push(json!({"jour": libelle, "entrees": [ligne]})),
        }
    }
    Ok(json!({"jours": jours, "total": entries.len()}))
}

/// Résumé en clair, destiné au modèle (et donc borné en taille).
pub fn narrate(db: &Db, window: &Window) -> Result<String> {
    let entries = build(db, window)?;
    if entries.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    let mut jour_courant = String::new();
    for entry in entries {
        let libelle = jour(entry.at);
        if libelle != jour_courant {
            out.push_str(&format!("\n{libelle} :\n"));
            jour_courant = libelle;
        }
        let detail = entry
            .detail
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        let ligne = format!("  {} — {}{}\n", heure(entry.at), entry.title, detail);
        if out.len() + ligne.len() > 4_000 {
            break;
        }
        out.push_str(&ligne);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn base() -> Db {
        let dir = std::env::temp_dir().join(format!("syn-timeline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db"), &"b".repeat(64)).unwrap()
    }

    #[test]
    fn la_chronologie_melange_les_sources_dans_lordre() {
        let db = base();
        let t = crate::db::now();
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES ('m1','mail','ref1','email','Devis toiture','De : Julie <julie@x.fr>\nÀ : paul@moi.fr\nObjet : Devis\n\n',?1,?1,'active')",
                params![t - 3600],
            )?;
            c.execute(
                "INSERT INTO events (id,source,source_ref,title,\"start\") VALUES ('e1','apple','ref2','Point équipe',?1)",
                params![t - 1800],
            )?;
            c.execute(
                "INSERT INTO actions_log (id,tool,input,risk_class,status,preview,created_at)
                 VALUES ('a1','mail.send','{}','floor','executed','Envoyer un mail à Julie',?1)",
                params![t - 600],
            )?;
            Ok(())
        })
        .unwrap();

        let entries = build(&db, &Window::last_days(7, 20)).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, "action");
        assert_eq!(entries[1].kind, "rendez_vous");
        assert_eq!(entries[2].kind, "mail_recu");
    }

    #[test]
    fn la_fenetre_filtre_par_sujet_et_par_type() {
        let db = base();
        let t = crate::db::now();
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES ('m1','mail','ref1','email','Devis toiture','De : Julie <julie@x.fr>\n',?1,?1,'active')",
                params![t - 3600],
            )?;
            c.execute(
                "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES ('m2','mail','ref2','email','Facture énergie','De : EDF <edf@x.fr>\n',?1,?1,'active')",
                params![t - 3500],
            )?;
            Ok(())
        })
        .unwrap();

        let mut window = Window::last_days(7, 20);
        window.about = Some("toiture".into());
        let entries = build(&db, &window).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Devis toiture");

        let mut window = Window::last_days(7, 20);
        window.kinds = Some(vec!["rendez_vous".into()]);
        assert!(build(&db, &window).unwrap().is_empty());
    }
}
