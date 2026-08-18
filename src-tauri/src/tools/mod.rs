//! Catalogue d'outils (doc maître §10). Le routeur reçoit ce catalogue ;
//! ajouter une capacité = ajouter un outil. Chaque outil déclare son side_effect ;
//! la classe de risque est calculée par la porte d'action (actions::classify).

pub mod attachments;
pub mod documents;
pub mod docx_edit;
pub mod ooxml;
pub mod pptx_edit;
pub mod reorganize;
pub mod xlsx_edit;

use crate::bus::Bus;
use crate::connectors::{calendar, people as people_conn, system as system_conn};
use crate::db::{new_id, now, Db};
use crate::error::{AppError, Result};
use crate::llm::{LlmClient, SideEffect, ToolSpec};
use crate::memory;
use crate::retrieval;
use crate::settings::Settings;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ToolCtx {
    pub db: Db,
    pub llm: Arc<dyn LlmClient>,
    pub bus: Bus,
    pub settings: Settings,
}

pub struct ToolResult {
    pub result: Value,
    pub undo: Option<Value>,
}

fn spec(
    name: &str,
    description: &str,
    props: Value,
    required: &[&str],
    side_effect: SideEffect,
) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": props,
            "required": required,
        }),
        side_effect,
    }
}

/// Outils utiles à une intention donnée.
///
/// Envoyer les 23 outils du catalogue à chaque itération coûtait **18,7 s par
/// appel** contre 1,15 s sans outil (mesuré sur llama3.1). Avec jusqu'à cinq
/// itérations, l'utilisateur attendait une minute pour un simple bonjour.
///
/// Ce n'est pas qu'une optimisation : proposer `files.reorganize` pendant la
/// rédaction d'un mail, c'est aussi inviter le modèle à se tromper d'outil.
/// La table ci-dessous relie une INTENTION à des CAPACITÉS — elle ne dépend
/// d'aucune formulation.
pub fn catalog_for(kind: crate::router::intent::Kind) -> Vec<ToolSpec> {
    use crate::router::intent::Kind;
    let retenus: &[&str] = match kind {
        Kind::MailSearch => &[
            "mail.search",
            "mail.list",
            "mail.open",
            "mail.attachments",
            "memory.query",
        ],
        Kind::MailCompose => &[
            "mail.search",
            "mail.draft",
            "mail.send",
            "people.resolve_email",
            "people.context",
            "memory.query",
        ],
        Kind::DocumentCreate => &[
            "document.create",
            "document.write",
            "document.edit",
            "document.open",
            "files.search",
            "memory.query",
        ],
        Kind::DeviceDiagnostic => &["system.diagnose", "memory.query"],
        Kind::FileSearch => &[
            "files.search",
            "cloud.search",
            "memory.query",
            "document.open",
        ],
        // Intention indéterminée : on garde de quoi percevoir et agir sur les
        // gestes courants, sans dérouler tout le catalogue.
        Kind::Conversation => &[
            "memory.query",
            "files.search",
            "cloud.search",
            "mail.search",
            "calendar.list",
            "calendar.create",
            "tasks.list",
            "tasks.create",
            "tasks.complete",
            "commitments.list",
            "people.context",
            "system.diagnose",
            "memory.remember",
            "document.create",
            "document.edit",
            "document.open",
            "files.reorganize",
        ],
    };
    catalog()
        .into_iter()
        .filter(|spec| retenus.contains(&spec.name.as_str()))
        .collect()
}

/// Outils exécutables mais JAMAIS proposés au modèle : Syn les déclenche
/// lui-même, sur un constat déterministe. `people.link_email` figurait au
/// catalogue et le modèle l'appelait sans arguments — d'où la carte « Retenir
/// que ? utilise l'adresse ? » vue par l'utilisateur.
pub fn catalog() -> Vec<ToolSpec> {
    vec![
        spec(
            "memory.query",
            "Recherche dans la mémoire de Syn (documents, mails, notes, faits appris). À utiliser pour toute question sur la vie numérique de l'utilisateur.",
            json!({"query": {"type": "string", "description": "requête en langage naturel"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "files.search",
            "Recherche parmi les fichiers indexés (contenu et métadonnées). Renvoie des chemins ouvrables.",
            json!({"query": {"type": "string"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "cloud.search",
            "Recherche dans les fichiers Google Drive et Microsoft OneDrive synchronisés. Renvoie des liens ouvrables et sourcés.",
            json!({"query": {"type": "string"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "files.reorganize",
            "Prépare un PLAN de rangement intelligent d'un fichier, dossier ou emplacement autorisé (simulation, rien n'est déplacé). Accepte un chemin exact ou un nom non ambigu. L'utilisateur revoit le plan une seule fois avant exécution.",
            json!({"target_dir": {"type": "string", "description": "fichier ou dossier cible, par nom ou chemin, dans le périmètre autorisé"}}),
            &["target_dir"],
            SideEffect::Read,
        ),
        spec(
            "files.move",
            "Déplace précisément un fichier ou dossier existant dans un dossier de destination existant. Utilise cet outil quand l'utilisateur dit naturellement « mets/déplace/range X dans Y ». Ne l'utilise PAS pour classer le contenu de X : dans ce cas utilise files.reorganize.",
            json!({
                "source": {"type": "string", "description": "nom ou chemin du fichier/dossier à déplacer"},
                "destination": {"type": "string", "description": "nom ou chemin du dossier dans lequel placer la source"}
            }),
            &["source", "destination"],
            SideEffect::WriteLocal,
        ),
        spec(
            "files.create_folder_and_move",
            "Crée un dossier de destination manquant puis y déplace un fichier précis. Action locale réversible, toujours proposée explicitement avant exécution.",
            json!({
                "source": {"type": "string", "description": "chemin exact du fichier à déplacer"},
                "destination": {"type": "string", "description": "chemin exact du dossier à créer"}
            }),
            &["source", "destination"],
            SideEffect::WriteLocal,
        ),
        spec(
            "document.create",
            "Crée un NOUVEAU document et l'enregistre réellement : sur le Mac (md, txt, csv, docx), dans Google Docs ou dans OneDrive au format Word. À utiliser dès que l'utilisateur demande d'écrire, rédiger ou créer un document, un compte rendu, une note ou un tableau.",
            json!({
                "title": {"type": "string", "description": "titre du document, il devient son nom de fichier"},
                "content": {"type": "string", "description": "contenu rédigé du document"},
                "location": {"type": "string", "enum": ["local", "google", "microsoft"], "description": "où l'enregistrer ; « local » par défaut, « google » pour Google Docs, « microsoft » pour un Word sur OneDrive"},
                "format": {"type": "string", "enum": ["md", "txt", "csv", "docx"], "description": "format local ; ignoré pour google et microsoft"},
                "folder": {"type": "string", "description": "dossier local de destination (nom ou chemin) ; Documents par défaut"},
                "open": {"type": "boolean", "description": "ouvrir le document juste après sa création"}
            }),
            &["title", "content"],
            SideEffect::WriteLocal,
        ),
        spec(
            "document.write",
            "Écrit dans un document texte local EXISTANT (md, txt, csv), en le complétant ou en le remplaçant. La version précédente est conservée pour permettre l'annulation.",
            json!({
                "target": {"type": "string", "description": "nom ou chemin du document à modifier"},
                "content": {"type": "string", "description": "texte à écrire"},
                "mode": {"type": "string", "enum": ["append", "replace"], "description": "« append » complète (défaut), « replace » remplace tout le contenu"}
            }),
            &["target", "content"],
            SideEffect::WriteLocal,
        ),
        spec(
            "document.edit",
            "Retouche un document Word EXISTANT en préservant sa mise en forme, ses images et ses styles. Opérations : mettre en forme des paragraphes (couleur, gras, italique, taille) en visant les titres, le corps, tout, ou ceux qui contiennent un texte ; remplacer un texte ; ajouter un paragraphe ; réserver l'emplacement d'une image que Syn ne sait pas produire.",
            json!({
                "target": {"type": "string", "description": "nom ou chemin du document à retoucher"},
                "operations": {
                    "type": "array",
                    "description": "liste d'opérations, appliquées dans l'ordre",
                    "items": {"type": "object", "properties": {
                        "op": {"type": "string", "enum": ["format", "replace", "append", "image_placeholder"]},
                        "scope": {"type": "string", "enum": ["titres", "corps", "tout", "contenant"], "description": "pour « format » : quels paragraphes"},
                        "contains": {"type": "string", "description": "pour scope « contenant » : le texte à repérer"},
                        "color": {"type": "string", "description": "couleur hexadécimale sans dièse, ex. 0000FF"},
                        "bold": {"type": "boolean"},
                        "italic": {"type": "boolean"},
                        "size_pt": {"type": "integer"},
                        "from": {"type": "string", "description": "pour « replace »"},
                        "to": {"type": "string", "description": "pour « replace »"},
                        "text": {"type": "string", "description": "pour « append »"},
                        "heading": {"type": "boolean", "description": "pour « append » : en faire un titre"},
                        "description": {"type": "string", "description": "pour « image_placeholder » : ce que l'image doit montrer"}
                    }}
                }
            }),
            &["target", "operations"],
            SideEffect::WriteLocal,
        ),
        spec(
            "document.open",
            "Ouvre un document dans l'application de l'utilisateur : chemin local, nom indexé, ou lien Google Drive / OneDrive déjà connu de Syn.",
            json!({"target": {"type": "string", "description": "nom, chemin ou lien du document à ouvrir"}}),
            &["target"],
            SideEffect::Read,
        ),
        spec(
            "mail.search",
            "Recherche dans les mails ingérés.",
            json!({"query": {"type": "string"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "mail.list",
            "Liste les derniers messages reçus, ou seulement les non lus. À utiliser quand l'utilisateur veut VOIR sa boîte, sans rien chercher de précis.",
            json!({
                "unread_only": {"type": "boolean", "description": "ne garder que les messages non lus"},
                "limit": {"type": "integer", "description": "nombre de messages, 10 par défaut"}
            }),
            &[],
            SideEffect::Read,
        ),
        spec(
            "mail.open",
            "Affiche le contenu complet d'un message déjà identifié, à partir de sa référence (source_ref).",
            json!({"source_ref": {"type": "string", "description": "référence du message, de la forme google:mail:… ou microsoft:mail:…"}}),
            &["source_ref"],
            SideEffect::Read,
        ),
        spec(
            "mail.attachments",
            "Importe les pièces jointes d'un message dans la conversation : Syn les télécharge, les lit, et peut ensuite répondre à leur sujet ou les modifier.",
            json!({"source_ref": {"type": "string", "description": "référence du message, de la forme google:mail:… ou microsoft:mail:…"}}),
            &["source_ref"],
            SideEffect::Read,
        ),
        spec(
            "mail.delete",
            "Met un message à la corbeille de sa messagerie. Réversible : le message reste récupérable chez le fournisseur.",
            json!({"source_ref": {"type": "string", "description": "référence du message à mettre à la corbeille"}}),
            &["source_ref"],
            SideEffect::WriteExternal,
        ),
        spec(
            "mail.draft",
            "Prépare un BROUILLON de mail (local, rien n'est envoyé).",
            json!({
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"},
                "via": {"type": "string", "enum": ["apple", "google", "microsoft"], "description": "service d’envoi ; choisir le compte demandé par l’utilisateur"}
            }),
            &["to", "subject", "body"],
            SideEffect::WriteLocal,
        ),
        spec(
            "mail.send",
            "Envoie un mail via Apple Mail, Gmail ou Microsoft Outlook. Utilise `via` selon le compte demandé. Action vers une personne réelle : TOUJOURS confirmée par l'utilisateur.",
            json!({
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"}
            }),
            &["to", "subject", "body"],
            SideEffect::WriteExternal,
        ),
        spec(
            "calendar.list",
            "Liste les événements du calendrier entre deux dates (ISO 8601).",
            json!({"from": {"type": "string"}, "to": {"type": "string"}}),
            &["from", "to"],
            SideEffect::Read,
        ),
        spec(
            "calendar.create",
            "Crée un événement. Avec invités : confirmation obligatoire (plancher).",
            json!({
                "title": {"type": "string"},
                "start": {"type": "string", "description": "ISO 8601"},
                "end": {"type": "string"},
                "location": {"type": "string"},
                "attendees": {"type": "array", "items": {"type": "string"}},
                "via": {"type": "string", "enum": ["apple", "google", "microsoft"], "description": "calendrier de destination"}
            }),
            &["title", "start"],
            SideEffect::WriteExternal,
        ),
        spec(
            "tasks.list",
            "Liste les tâches (statut open par défaut).",
            json!({"status": {"type": "string", "enum": ["open", "done", "all"]}}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "tasks.create",
            "Crée une tâche locale.",
            json!({
                "title": {"type": "string"},
                "due": {"type": "string", "description": "ISO 8601, optionnel"},
                "priority": {"type": "string", "enum": ["haute", "normale", "basse"]}
            }),
            &["title"],
            SideEffect::WriteLocal,
        ),
        spec(
            "tasks.complete",
            "Marque une tâche comme faite.",
            json!({"id": {"type": "string"}}),
            &["id"],
            SideEffect::WriteLocal,
        ),
        spec(
            "commitments.list",
            "Liste les engagements pris ou reçus (promesses extraites des échanges).",
            json!({}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "people.context",
            "Rassemble le contexte connu sur une personne (échanges, fichiers, événements liés).",
            json!({"name": {"type": "string"}}),
            &["name"],
            SideEffect::Read,
        ),
        spec(
            "people.resolve_email",
            "Résout un nom de destinataire vers une adresse email connue. À appeler AVANT mail.send quand l'utilisateur donne un nom plutôt qu'une adresse. Si aucun résultat ou plusieurs résultats sont renvoyés, demande une précision ; n'invente jamais d'adresse.",
            json!({"name": {"type": "string", "description": "nom donné par l'utilisateur"}}),
            &["name"],
            SideEffect::Read,
        ),
        spec(
            "photos.search",
            "Recherche de photos par métadonnées EXIF (date, lieu GPS) et nom. Renvoie des candidates à confirmer.",
            json!({
                "query": {"type": "string"},
                "from": {"type": "string", "description": "date ISO, optionnel"},
                "to": {"type": "string"}
            }),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "system.diagnose",
            "Diagnostique l'état de la machine (CPU, mémoire, disque, température, batterie) et explique les causes probables.",
            json!({}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "memory.remember",
            "Mémorise un fait durable dit par l'utilisateur (ex. « mon checkup est mardi 15h »).",
            json!({"fact": {"type": "string"}}),
            &["fact"],
            SideEffect::WriteLocal,
        ),
    ]
}

/// Traduit les opérations décrites par le modèle en opérations de document.
///
/// Une opération incomprise est REFUSÉE, jamais approximée : mieux vaut dire
/// « je n'ai pas su faire » que retoucher un document de travers.
fn parse_edit_operations(value: &Value) -> Result<Vec<docx_edit::Operation>> {
    let items = value
        .as_array()
        .ok_or_else(|| AppError::Invalid("aucune opération demandée".into()))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let op = item["op"].as_str().unwrap_or_default().to_lowercase();
        match op.as_str() {
            "format" => {
                let target = match item["scope"].as_str().unwrap_or("tout") {
                    "titres" | "titles" | "headings" => docx_edit::Target::Headings,
                    "corps" | "body" => docx_edit::Target::Body,
                    "contenant" | "containing" => docx_edit::Target::Containing(
                        item["contains"].as_str().unwrap_or_default().to_string(),
                    ),
                    _ => docx_edit::Target::All,
                };
                out.push(docx_edit::Operation::Format {
                    target,
                    formatting: docx_edit::Formatting {
                        color: item["color"].as_str().map(normalized_color),
                        bold: item["bold"].as_bool(),
                        italic: item["italic"].as_bool(),
                        size_pt: item["size_pt"].as_u64().map(|value| value as u32),
                    },
                });
            }
            "replace" => out.push(docx_edit::Operation::Replace {
                from: item["from"].as_str().unwrap_or_default().to_string(),
                to: item["to"].as_str().unwrap_or_default().to_string(),
            }),
            "append" => out.push(docx_edit::Operation::Append {
                text: item["text"].as_str().unwrap_or_default().to_string(),
                heading: item["heading"].as_bool().unwrap_or(false),
            }),
            "image_placeholder" | "image" => out.push(docx_edit::Operation::ImagePlaceholder {
                description: item["description"]
                    .as_str()
                    .or_else(|| item["text"].as_str())
                    .unwrap_or("image à insérer")
                    .to_string(),
            }),
            autre => {
                return Err(AppError::Invalid(format!(
                    "Je ne sais pas faire « {autre} » sur un document."
                )))
            }
        }
    }
    if out.is_empty() {
        return Err(AppError::Invalid("aucune opération demandée".into()));
    }
    Ok(out)
}

/// Une couleur telle que le modèle peut l'écrire : « #0000FF », « bleu », « blue ».
fn normalized_color(value: &str) -> String {
    let brut = value.trim().trim_start_matches('#');
    if brut.len() == 6 && brut.chars().all(|c| c.is_ascii_hexdigit()) {
        return brut.to_uppercase();
    }
    match crate::db::fold(brut).as_str() {
        "bleu" | "blue" => "0000FF",
        "rouge" | "red" => "FF0000",
        "vert" | "green" => "008000",
        "noir" | "black" => "000000",
        "gris" | "gray" | "grey" => "808080",
        "orange" => "FFA500",
        "violet" | "purple" => "800080",
        "jaune" | "yellow" => "FFFF00",
        _ => "000000",
    }
    .to_string()
}

/// Découpe une référence de message en (fournisseur, identifiant).
///
/// La référence vient de l'interface, donc indirectement d'un contenu indexé :
/// on ne lui fait pas confiance sur parole, on vérifie sa forme.
fn split_mail_ref(reference: &str) -> Result<(&'static str, &str)> {
    let (prefixe, id) = reference
        .split_once(":mail:")
        .ok_or_else(|| AppError::Invalid("référence de message invalide".into()))?;
    let provider = match prefixe {
        "google" => "google",
        "microsoft" => "microsoft",
        _ => return Err(AppError::Invalid("messagerie inconnue".into())),
    };
    if id.trim().is_empty() || id.contains('/') {
        return Err(AppError::Invalid("identifiant de message invalide".into()));
    }
    Ok((provider, id))
}

/// Compte rendu lisible d'une action exécutée.
///
/// Sans lui, l'interface affichait le résultat BRUT de l'outil dans la
/// conversation — l'utilisateur a vu `{"status":"envoyé","to":…,"via":"google"}`
/// s'écrire à la place d'une phrase. Un résultat d'outil est une donnée de
/// travail, pas une réponse.
/// `vouvoie` : la forme d'adresse choisie par l'utilisateur (Personnalisation
/// ou règle). Un compte rendu écrit en dur qui tutoie un utilisateur qui a
/// demandé le vouvoiement est une petite trahison de sa consigne.
pub fn outcome_summary(tool: &str, result: &Value, vouvoie: bool) -> String {
    let field = |key: &str| result[key].as_str().unwrap_or_default().to_string();
    let pick = |tu: &'static str, vous: &'static str| if vouvoie { vous } else { tu };
    // Un compte rendu déjà rédigé par l'outil prime toujours.
    for key in ["display_report", "report"] {
        if let Some(text) = result[key].as_str().filter(|text| !text.trim().is_empty()) {
            return text.to_string();
        }
    }
    match tool {
        "mail.send" => {
            let service = crate::connectors::mail::channel_label(&field("via"));
            format!(
                "C'est fait, le mail a été correctement envoyé. {} dans {} éléments envoyés sur {service}.",
                pick("Tu peux le retrouver", "Vous pouvez le retrouver"),
                pick("tes", "vos"),
            )
        }
        "mail.draft" => format!("Brouillon enregistré dans {}.", field("saved_in")),
        "mail.attachments" => field("report"),
        "mail.delete" => pick(
            "Le message est dans la corbeille. Tu peux encore le récupérer depuis ta messagerie.",
            "Le message est dans la corbeille. Vous pouvez encore le récupérer depuis votre messagerie.",
        )
        .to_string(),
        "people.link_email" => format!(
            "C'est retenu : {} utilise l'adresse {}.",
            field("name"),
            field("email")
        ),
        "document.create" => format!(
            "Document créé dans {} : {}.",
            field("service"),
            field("name")
        ),
        "document.edit" => field("report"),
        "document.write" => format!("Document {} : {}.", field("mode"), field("path")),
        "calendar.create" => "Événement ajouté à ton agenda.".into(),
        "tasks.create" => "Tâche créée.".into(),
        "tasks.complete" => "Tâche marquée comme faite.".into(),
        "memory.remember" => "C'est mémorisé.".into(),
        // Jamais de JSON par défaut : mieux vaut une phrase vague qu'une
        // structure interne affichée à l'utilisateur.
        _ => "Action effectuée.".into(),
    }
}

/// Aperçu lisible pour la confirmation (les confirmations d'actions graves
/// doivent être claires, explicites, non pré-cochées — Sécurité §6).
pub fn preview_for(tool: &str, args: &Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    match tool {
        "mail.send" => format!(
            "Envoyer un mail à {} depuis {} — objet : « {} »\n{}",
            s("to"),
            match args["via"].as_str().unwrap_or("apple") {
                "google" => "Gmail",
                "microsoft" => "Outlook",
                _ => "Apple Mail",
            },
            s("subject"),
            s("body").chars().take(500).collect::<String>()
        ),
        "mail.delete" => format!(
            "Mettre à la corbeille le message {} (récupérable chez le fournisseur)",
            s("source_ref")
        ),
        "mail.draft" => format!(
            "Créer un brouillon pour {} — objet : « {} »",
            s("to"),
            s("subject")
        ),
        "calendar.create" => {
            let attendees = args["attendees"].as_array().map(|a| a.len()).unwrap_or(0);
            if attendees > 0 {
                format!(
                    "Créer l'événement « {} » le {} avec {} invité(s)",
                    s("title"),
                    s("start"),
                    attendees
                )
            } else {
                format!("Créer l'événement « {} » le {}", s("title"), s("start"))
            }
        }
        "tasks.create" => format!("Créer la tâche « {} »", s("title")),
        "tasks.complete" => "Marquer une tâche comme faite".into(),
        "files.apply_reorganize_plan" => "Exécuter le plan de rangement validé".into(),
        "files.move" => format!("Déplacer « {} » dans « {} »", s("source"), s("destination")),
        "files.create_folder_and_move" => format!(
            "Créer « {} » puis y déplacer « {} »",
            s("destination"),
            s("source")
        ),
        "document.create" => {
            let service = match args["location"].as_str().unwrap_or("local") {
                "google" => "Google Docs",
                "microsoft" => "OneDrive (Word)",
                _ => "le Mac",
            };
            format!(
                "Créer le document « {} » dans {service}\n{}",
                s("title"),
                s("content").chars().take(500).collect::<String>()
            )
        }
        "document.edit" => {
            format!(
            "Retoucher le document « {} » — {} opération(s), la version précédente est sauvegardée",
            s("target"),
            args["operations"].as_array().map(|items| items.len()).unwrap_or(0)
        )
        }
        "document.write" => format!(
            "{} le document « {} »\n{}",
            if args["mode"].as_str() == Some("replace") {
                "Remplacer le contenu de"
            } else {
                "Compléter"
            },
            s("target"),
            s("content").chars().take(500).collect::<String>()
        ),
        "document.open" => format!("Ouvrir « {} »", s("target")),
        "people.link_email" => {
            format!("Retenir que {} utilise l'adresse {}", s("name"), s("email"))
        }
        "memory.remember" => format!("Mémoriser : « {} »", s("fact")),
        _ => format!("{tool} {args}"),
    }
}

/// Exécution effective (appelée pour les lectures, ou après passage de la porte).
pub async fn execute(ctx: &ToolCtx, tool: &str, args: &Value) -> Result<ToolResult> {
    let connector = match tool.split('.').next().unwrap_or("") {
        "system" => Some("system"),
        _ => None,
    };
    if let Some(id) = connector {
        if !crate::connectors::is_connected(&ctx.db, id) {
            return Err(AppError::Security(format!(
                "Le connecteur {id} n'est pas activé."
            )));
        }
    }
    match tool {
        "memory.query" | "files.search" | "mail.search" | "cloud.search" => {
            let query = args["query"].as_str().unwrap_or("");
            let local_results = if tool == "files.search" {
                retrieval::search_lexical_source(&ctx.db, query, 10, "files").await?
            } else if tool == "mail.search" {
                retrieval::search_source(&ctx.db, &ctx.llm, query, 10, "mail").await?
            } else if tool == "cloud.search" {
                retrieval::search_source(&ctx.db, &ctx.llm, query, 10, "cloud").await?
            } else {
                retrieval::search(&ctx.db, &ctx.llm, query, 10).await?
            };
            let mut results = local_results
                .into_iter()
                .filter_map(|result| serde_json::to_value(result).ok())
                .collect::<Vec<_>>();
            if tool == "mail.search" {
                results.extend(crate::connectors::external::live_search("mail", query).await);
            } else if tool == "cloud.search" {
                results.extend(crate::connectors::external::live_search("cloud", query).await);
            }
            crate::security::log_access(
                &ctx.db,
                tool.split('.').next().unwrap_or("memory"),
                "search",
                Some(query),
            );
            // Transparence : sans état de l'index, « aucun résultat » est
            // indiscernable de « pas encore indexé » — et le modèle conclut à tort.
            let (files_count, last_ingest): (i64, Option<i64>) = ctx.db.with(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*), MAX(ingested_at) FROM items WHERE source='files' AND status='active'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((0, None)))
            })?;
            let indexing_recent = last_ingest.map(|t| now() - t < 180).unwrap_or(false);
            Ok(ToolResult {
                result: json!({
                    "results": results,
                    "index_status": {
                        "fichiers_indexes": files_count,
                        "indexation_en_cours_probable": indexing_recent,
                        "note": if results.is_empty() && (indexing_recent || files_count == 0) {
                            "L'index est vide ou encore en construction : dis-le à l'utilisateur au lieu de conclure que le document n'existe pas."
                        } else if results.is_empty() {
                            "Aucun résultat : reformule avec d'autres termes (synonymes, singulier/pluriel) avant de conclure."
                        } else { "" }
                    }
                }),
                undo: None,
            })
        }

        "files.reorganize" => {
            let target = args["target_dir"].as_str().unwrap_or("");
            let plan = reorganize::build_plan(&ctx.db, &ctx.llm, target).await?;
            let plan_id = new_id();
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO reorganize_plans (id, plan, status, created_at) VALUES (?1,?2,'pending',?3)",
                    rusqlite::params![plan_id, serde_json::to_string(&plan)?, now()],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({"plan_id": plan_id, "plan": plan}),
                undo: None,
            })
        }

        "files.apply_reorganize_plan" => {
            let plan_id = args["plan_id"]
                .as_str()
                .ok_or(AppError::Invalid("plan_id requis".into()))?;
            let plan: reorganize::Plan = ctx.db.with(|c| {
                let raw: String = c
                    .query_row(
                        "SELECT plan FROM reorganize_plans WHERE id=?1 AND status='pending'",
                        rusqlite::params![plan_id],
                        |r| r.get(0),
                    )
                    .map_err(|_| AppError::NotFound("plan introuvable ou déjà exécuté".into()))?;
                serde_json::from_str(&raw)
                    .map_err(|_| AppError::Invalid("plan de rangement invalide".into()))
            })?;
            let (report, undo) = reorganize::execute_plan(&plan)?;
            let display_report = reorganize::execution_report(&plan, &undo, &report);
            ctx.db.with(|c| {
                c.execute(
                    "UPDATE reorganize_plans SET status='executed' WHERE id=?1",
                    rusqlite::params![plan_id],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({
                    "report": report,
                    "display_report": display_report,
                    "plan": plan,
                    "execution": {
                        "moves": undo["moves"].clone(),
                        "created_dirs": undo["created_dirs"].clone()
                    }
                }),
                undo: Some(undo),
            })
        }

        "files.move" => {
            let source = args["source"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("source requise".into()))?;
            let destination = args["destination"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("destination requise".into()))?;
            let (report, undo) = reorganize::move_location(&ctx.db, source, destination)?;
            crate::security::log_access(&ctx.db, "files", "move", Some(source));
            Ok(ToolResult {
                result: json!({"report": report}),
                undo: Some(undo),
            })
        }

        "files.create_folder_and_move" => {
            let source = args["source"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("source requise".into()))?;
            let destination = args["destination"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("destination requise".into()))?;
            let (report, undo) = reorganize::create_folder_and_move(&ctx.db, source, destination)?;
            crate::security::log_access(&ctx.db, "files", "create_folder_and_move", Some(source));
            Ok(ToolResult {
                result: json!({"report": report}),
                undo: Some(undo),
            })
        }

        "document.create" => {
            let title = args["title"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::Invalid("titre du document requis".into()))?;
            let content = args["content"].as_str().unwrap_or_default();
            let location = args["location"].as_str().unwrap_or("local").to_lowercase();
            match location.as_str() {
                "google" | "microsoft" => {
                    let created =
                        crate::connectors::external::create_document(&location, title, content)
                            .await?;
                    crate::security::log_access(&ctx.db, &location, "create_document", Some(title));
                    // Le document distant est immédiatement présent dans le cache :
                    // l'utilisateur peut le retrouver sans attendre la synchro.
                    if let Some(source_ref) = created["source_ref"].as_str() {
                        memory::upsert_item(
                            &ctx.db,
                            &memory::Item {
                                id: String::new(),
                                source: "cloud".into(),
                                source_ref: source_ref.into(),
                                r#type: "document".into(),
                                title: Some(title.to_string()),
                                body: Some(content.to_string()),
                                created_at: Some(now()),
                                ingested_at: now(),
                                hash: None,
                                path: created["url"].as_str().map(str::to_string),
                                mime: None,
                                size: None,
                                mtime: Some(now()),
                                status: "active".into(),
                            },
                        )?;
                    }
                    if args["open"].as_bool() == Some(true) {
                        if let Some(url) = created["url"].as_str() {
                            let _ = documents::open_target(&ctx.db, url);
                        }
                    }
                    Ok(ToolResult {
                        result: created,
                        undo: None,
                    })
                }
                "local" | "mac" | "" => {
                    let document = documents::create_local(
                        &ctx.db,
                        &ctx.llm,
                        &ctx.bus,
                        &ctx.settings.embed_model,
                        title,
                        content,
                        args["format"].as_str().unwrap_or("md"),
                        args["folder"].as_str(),
                    )
                    .await?;
                    let path = document.path.to_string_lossy().to_string();
                    crate::security::log_access(&ctx.db, "files", "create_document", Some(&path));
                    if args["open"].as_bool() == Some(true) {
                        let _ = documents::open_target(&ctx.db, &path);
                    }
                    Ok(ToolResult {
                        result: json!({
                            "service": "Mac",
                            "path": path,
                            "name": document.path.file_name().map(|name| name.to_string_lossy()),
                        }),
                        undo: Some(json!({"kind": "delete_file", "path": path})),
                    })
                }
                other => Err(AppError::Invalid(format!(
                    "Emplacement « {other} » inconnu : local, google ou microsoft."
                ))),
            }
        }

        "document.edit" => {
            let target = args["target"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("document cible requis".into()))?;
            let operations = parse_edit_operations(&args["operations"])?;
            // Un fichier Google se retouche par son API : elle applique des
            // opérations structurées et préserve le reste elle-même. Aucune
            // sauvegarde locale n'a de sens — Google conserve l'historique des
            // versions, et c'est lui qui fait foi.
            if let Some(file_id) = documents::locate_google(&ctx.db, target)? {
                let mime = crate::connectors::external::drive_mime(&file_id).await?;
                if crate::connectors::gsuite::family_of(&mime).is_some() {
                    let (faits, applique) =
                        crate::connectors::gsuite::edit(&file_id, &mime, &operations).await?;
                    crate::security::log_access(&ctx.db, "google", "edit_document", Some(target));
                    if !applique {
                        return Err(AppError::Invalid(
                            "Aucun passage de ce document ne correspond à ce que tu demandes."
                                .into(),
                        ));
                    }
                    return Ok(ToolResult {
                        result: json!({
                            "report": format!("« {target} » retouché chez Google : {faits}. L'historique des versions de Google garde l'état précédent."),
                        }),
                        undo: None,
                    });
                }
            }
            let (report, undo) = documents::edit_local(&ctx.db, target, &operations)?;
            crate::security::log_access(&ctx.db, "files", "edit_document", Some(target));
            if let Some(path) = report["path"].as_str() {
                crate::connectors::files::index_file(
                    &ctx.db,
                    &ctx.llm,
                    &ctx.bus,
                    &ctx.settings.embed_model,
                    std::path::Path::new(path),
                )
                .await
                .ok();
            }
            Ok(ToolResult {
                result: report,
                undo: Some(undo),
            })
        }

        "document.write" => {
            let target = args["target"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("document cible requis".into()))?;
            let content = args["content"].as_str().unwrap_or_default();
            let (report, undo) = documents::write_local(
                &ctx.db,
                target,
                content,
                args["mode"].as_str().unwrap_or("append"),
            )?;
            crate::security::log_access(&ctx.db, "files", "write_document", Some(target));
            // Le document modifié doit rester cherchable sur son nouveau contenu.
            if let Some(path) = report["path"].as_str() {
                crate::connectors::files::index_file(
                    &ctx.db,
                    &ctx.llm,
                    &ctx.bus,
                    &ctx.settings.embed_model,
                    std::path::Path::new(path),
                )
                .await?;
            }
            Ok(ToolResult {
                result: report,
                undo: Some(undo),
            })
        }

        "document.open" => {
            let target = args["target"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("document à ouvrir requis".into()))?;
            let result = documents::open_target(&ctx.db, target)?;
            crate::security::log_access(&ctx.db, "files", "open_document", Some(target));
            Ok(ToolResult { result, undo: None })
        }

        "mail.list" => {
            let unread = args["unread_only"].as_bool().unwrap_or(false);
            let limit = args["limit"].as_u64().unwrap_or(10) as usize;
            let mut messages = Vec::new();
            for (provider, _) in crate::connectors::mail::available_channels(&ctx.db) {
                if provider == "apple" {
                    continue;
                }
                if let Ok(found) =
                    crate::connectors::external::list_mail(provider, unread, limit).await
                {
                    messages.extend(found);
                }
            }
            crate::security::log_access(&ctx.db, "mail", "list", None);
            Ok(ToolResult {
                result: json!({"messages": messages}),
                undo: None,
            })
        }

        "mail.attachments" => {
            let reference = args["source_ref"].as_str().unwrap_or("").trim();
            let (provider, id) = split_mail_ref(reference)?;
            if !crate::connectors::is_connected(&ctx.db, provider) {
                return Err(AppError::Security(format!(
                    "Le connecteur {provider} n’est pas synchronisé."
                )));
            }
            let fichiers = crate::connectors::external::download_attachments(provider, id).await?;
            if fichiers.is_empty() {
                return Ok(ToolResult {
                    result: json!({"report": "Ce message ne contient aucune pièce jointe."}),
                    undo: None,
                });
            }
            // Chaque pièce jointe devient un document de la conversation : Syn
            // la lit une fois, et peut ensuite répondre à son sujet.
            let session = args["_syn_session"].as_str().unwrap_or_default();
            let mut noms = Vec::new();
            for chemin in &fichiers {
                if !session.is_empty() {
                    attachments::attach(&ctx.db, session, chemin)?;
                }
                noms.push(
                    chemin
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("pièce jointe")
                        .to_string(),
                );
            }
            crate::security::log_access(&ctx.db, provider, "mail.attachments", Some(reference));
            Ok(ToolResult {
                result: json!({
                    "report": format!("{} pièce(s) jointe(s) importée(s) : {}. Je peux maintenant répondre à leur sujet ou les modifier.", noms.len(), noms.join(", ")),
                    "fichiers": noms,
                }),
                undo: None,
            })
        }

        "mail.open" | "mail.delete" => {
            let reference = args["source_ref"].as_str().unwrap_or("").trim();
            let (provider, id) = split_mail_ref(reference)?;
            if !crate::connectors::is_connected(&ctx.db, provider) {
                return Err(AppError::Security(format!(
                    "Le connecteur {provider} n’est pas synchronisé."
                )));
            }
            if tool == "mail.open" {
                let message = crate::connectors::external::read_mail(provider, id).await?;
                crate::security::log_access(&ctx.db, provider, "mail.open", Some(reference));
                return Ok(ToolResult {
                    result: message,
                    undo: None,
                });
            }
            let result = crate::connectors::external::trash_mail(provider, id).await?;
            crate::security::log_access(&ctx.db, provider, "mail.delete", Some(reference));
            Ok(ToolResult { result, undo: None })
        }

        "mail.draft" => {
            let id = new_id();
            let (to, subject, body) = (
                args["to"].as_str().unwrap_or(""),
                args["subject"].as_str().unwrap_or(""),
                args["body"].as_str().unwrap_or(""),
            );
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1, 'mail', ?2, 'draft', ?3, ?4, ?5, 'active')",
                    rusqlite::params![id, format!("draft:{id}"), format!("Brouillon → {to} : {subject}"),
                        format!("À : {to}\nObjet : {subject}\n\n{body}"), now()],
                )?;
                Ok(())
            })?;
            // Vrai brouillon dans Apple Mail quand il est disponible : l'utilisateur
            // le retrouve dans Mail ▸ Brouillons, prêt à relire et envoyer.
            let mut where_saved = "mémoire de Syn";
            if cfg!(target_os = "macos") && crate::connectors::mail::native_available() {
                let script = apple_mail_script(to, subject, body, false);
                if tokio::task::spawn_blocking(move || run_osascript(&script))
                    .await
                    .map_err(|e| AppError::Other(e.to_string()))?
                    .is_ok()
                {
                    where_saved = "Apple Mail (dossier Brouillons)";
                }
            }
            Ok(ToolResult {
                result: json!({"status": "brouillon créé", "draft_id": id, "to": to, "subject": subject, "saved_in": where_saved}),
                undo: Some(json!({"kind": "delete_item", "id": id})),
            })
        }

        "mail.send" => {
            // Envoi réel via Apple Mail (compte par défaut de l'utilisateur).
            // Toujours derrière le plancher : on n'arrive ici qu'après confirmation.
            if args["_syn_preflight_v1"].as_bool() != Some(true) {
                return Err(AppError::Security(
                    "Cette demande d'envoi est ancienne ou n'a pas validé le destinataire et le contenu. Recompose le mail avant de l'envoyer.".into(),
                ));
            }
            let (to, subject, body) = (
                args["to"].as_str().unwrap_or("").trim().to_string(),
                args["subject"].as_str().unwrap_or("").to_string(),
                args["body"].as_str().unwrap_or("").to_string(),
            );
            if !(to.contains('@') && to.contains('.')) {
                return Err(AppError::Invalid(format!(
                    "Destinataire invalide : « {to} »."
                )));
            }
            if subject.trim().is_empty() || body.trim().is_empty() {
                return Err(AppError::Invalid(
                    "Un objet et un contenu non vides sont requis avant l'envoi.".into(),
                ));
            }
            let via = args["via"].as_str().unwrap_or("apple");
            if matches!(via, "google" | "microsoft") {
                if !crate::connectors::is_connected(&ctx.db, via) {
                    return Err(AppError::Security(format!(
                        "Le connecteur {via} n’est pas synchronisé."
                    )));
                }
                let result =
                    crate::connectors::external::send_mail(via, &to, &subject, &body).await?;
                crate::security::log_access(&ctx.db, via, "mail.send", Some(&to));
                return Ok(ToolResult { result, undo: None });
            }
            if !cfg!(target_os = "macos") || !crate::connectors::mail::native_available() {
                return Err(AppError::Invalid(
                    "L'envoi passe par Apple Mail, indisponible sur cette machine. Le brouillon reste possible.".into(),
                ));
            }
            let script = apple_mail_script(&to, &subject, &body, true);
            tokio::task::spawn_blocking(move || run_osascript(&script))
                .await
                .map_err(|e| AppError::Other(e.to_string()))??;
            crate::security::log_access(&ctx.db, "mail", "send", Some(&to));
            Ok(ToolResult {
                result: json!({"status": "envoyé", "via": "Apple Mail", "to": to, "subject": subject}),
                undo: None,
            })
        }

        "calendar.list" => {
            let (from, to) = (
                args["from"].as_str().unwrap_or(""),
                args["to"].as_str().unwrap_or(""),
            );
            let events = calendar::list_range(&ctx.db, from, to)?;
            Ok(ToolResult {
                result: json!({ "events": events }),
                undo: None,
            })
        }

        "calendar.create" => {
            if let Some(via @ ("google" | "microsoft")) = args["via"].as_str() {
                if !crate::connectors::is_connected(&ctx.db, via) {
                    return Err(AppError::Security(format!(
                        "Le connecteur {via} n’est pas synchronisé."
                    )));
                }
                let event = crate::connectors::external::create_event(via, args).await?;
                crate::security::log_access(
                    &ctx.db,
                    via,
                    "calendar.create",
                    event["event"]["id"].as_str(),
                );
                return Ok(ToolResult {
                    result: event,
                    undo: None,
                });
            }
            let ev = calendar::create(&ctx.db, args)?;
            Ok(ToolResult {
                result: json!({"status": "événement créé", "event": ev.clone()}),
                undo: Some(json!({"kind": "delete_event", "id": ev["id"]})),
            })
        }

        "tasks.list" => {
            let status = args["status"].as_str().unwrap_or("open");
            let tasks = ctx.db.with(|c| {
                let sql = if status == "all" {
                    "SELECT id, title, due, status, priority FROM tasks ORDER BY due IS NULL, due LIMIT 100"
                } else {
                    "SELECT id, title, due, status, priority FROM tasks WHERE status = ?1 ORDER BY due IS NULL, due LIMIT 100"
                };
                let mut stmt = c.prepare(sql)?;
                let map = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "due": r.get::<_, Option<i64>>(2)?,
                        "status": r.get::<_, String>(3)?,
                        "priority": r.get::<_, Option<String>>(4)?,
                    }))
                };
                let mut out = vec![];
                if status == "all" {
                    let rows = stmt.query_map([], map)?;
                    for r in rows {
                        out.push(r?);
                    }
                } else {
                    let rows = stmt.query_map([status], map)?;
                    for r in rows {
                        out.push(r?);
                    }
                }
                Ok(out)
            })?;
            Ok(ToolResult {
                result: json!({ "tasks": tasks }),
                undo: None,
            })
        }

        "tasks.create" => {
            let title = args["title"]
                .as_str()
                .ok_or(AppError::Invalid("titre requis".into()))?;
            let due = args["due"].as_str().and_then(parse_iso);
            let id = memory::create_task(
                &ctx.db,
                title,
                due,
                args["priority"].as_str(),
                "conversation",
            )?;
            // Miroir Rappels : la tâche existe aussi dans l'app Rappels du Mac.
            let mut native = false;
            if let Some(native_id) = crate::connectors::reminders::create_native(title, due) {
                let _ = ctx.db.with(|c| {
                    c.execute(
                        "UPDATE tasks SET external_ref=?2 WHERE id=?1",
                        rusqlite::params![id, native_id],
                    )?;
                    Ok(())
                });
                native = true;
            }
            Ok(ToolResult {
                result: json!({"status": "tâche créée", "id": id, "title": title, "rappel_macos": native}),
                undo: Some(json!({"kind": "delete_task", "id": id})),
            })
        }

        "tasks.complete" => {
            let id = args["id"]
                .as_str()
                .ok_or(AppError::Invalid("id requis".into()))?;
            let external: Option<String> = ctx.db.with(|c| {
                Ok(c.query_row(
                    "SELECT external_ref FROM tasks WHERE id=?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap_or(None))
            })?;
            ctx.db.with(|c| {
                c.execute(
                    "UPDATE tasks SET status='done' WHERE id=?1",
                    rusqlite::params![id],
                )?;
                Ok(())
            })?;
            if let Some(ext) = external {
                crate::connectors::reminders::complete_native(&ext);
            }
            Ok(ToolResult {
                result: json!({"status": "tâche terminée"}),
                undo: Some(json!({"kind": "reopen_task", "id": id})),
            })
        }

        "commitments.list" => {
            let list = ctx.db.with(|c| {
                let mut stmt = c.prepare(
                    "SELECT co.id, co.text, co.direction, co.due, co.status, p.name
                     FROM commitments co LEFT JOIN people p ON p.id = co.person_id
                     WHERE co.status='open' ORDER BY co.due IS NULL, co.due LIMIT 50",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "text": r.get::<_, String>(1)?,
                        "direction": r.get::<_, Option<String>>(2)?,
                        "due": r.get::<_, Option<i64>>(3)?,
                        "status": r.get::<_, String>(4)?,
                        "person": r.get::<_, Option<String>>(5)?,
                    }))
                })?;
                let mut out = vec![];
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })?;
            Ok(ToolResult {
                result: json!({ "commitments": list }),
                undo: None,
            })
        }

        "people.context" => {
            let name = args["name"].as_str().unwrap_or("");
            let context = people_conn::context(&ctx.db, name)?;
            crate::security::log_access(&ctx.db, "people", "context", Some(name));
            Ok(ToolResult {
                result: context,
                undo: None,
            })
        }

        "people.link_email" => {
            let name = args["name"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::Invalid("nom de la personne requis".into()))?;
            let email = args["email"]
                .as_str()
                .map(str::trim)
                .filter(|value| value.contains('@') && value.contains('.'))
                .ok_or_else(|| AppError::Invalid("adresse mail invalide".into()))?;
            let person_id =
                memory::find_or_create_person(&ctx.db, name, Some(&email.to_lowercase()), None)?;
            crate::security::log_access(&ctx.db, "people", "link_email", Some(name));
            Ok(ToolResult {
                result: json!({
                    "status": "retenu",
                    "name": name,
                    "email": email.to_lowercase(),
                }),
                undo: Some(json!({
                    "kind": "unlink_person_email",
                    "person_id": person_id,
                    "email": email.to_lowercase(),
                })),
            })
        }

        "people.resolve_email" => {
            let name = args["name"].as_str().unwrap_or("").trim();
            let result = people_conn::resolve_email(&ctx.db, name)?;
            // Une résolution INFRUCTUEUSE est notée à part : c'est le nom que
            // l'utilisateur cherchait sans que Syn le connaisse. Si un envoi
            // aboutit ensuite, c'est ce nom qu'on proposera d'associer — sans
            // avoir eu à interpréter une phrase.
            let operation = if result["resolved"].as_bool() == Some(true) {
                "resolve_email"
            } else {
                "resolve_email_unresolved"
            };
            crate::security::log_access(&ctx.db, "people", operation, Some(name));
            Ok(ToolResult { result, undo: None })
        }

        "photos.search" => {
            let query = args["query"].as_str().unwrap_or("");
            let photos = photos_search(&ctx.db, query, args["from"].as_str(), args["to"].as_str())?;
            Ok(ToolResult {
                result: json!({ "candidates": photos, "note": "candidates classées — à confirmer visuellement" }),
                undo: None,
            })
        }

        "system.diagnose" => {
            let snapshot = system_conn::snapshot();
            let explanation = system_conn::diagnose(&snapshot);
            crate::security::log_access(&ctx.db, "system", "diagnose", None);
            Ok(ToolResult {
                result: json!({ "snapshot": snapshot, "explanation": explanation }),
                undo: None,
            })
        }

        "memory.remember" => {
            let fact = args["fact"]
                .as_str()
                .ok_or(AppError::Invalid("fait requis".into()))?;
            let id = new_id();
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1, 'conversation', ?2, 'fact', ?3, ?4, ?5, 'active')",
                    rusqlite::params![id, format!("fact:{id}"), fact.chars().take(80).collect::<String>(), fact, now()],
                )?;
                Ok(())
            })?;
            // Embedding pour le retrieval futur.
            if let Ok(vecs) = ctx.llm.embed(&[fact.to_string()]).await {
                if let Some(v) = vecs.first() {
                    memory::replace_embeddings(
                        &ctx.db,
                        &id,
                        &ctx.settings.embed_model,
                        &[(fact.to_string(), Some(crate::llm::vec_to_blob(v)))],
                    )?;
                }
            }
            Ok(ToolResult {
                result: json!({"status": "mémorisé", "id": id}),
                undo: Some(json!({"kind": "delete_item", "id": id})),
            })
        }

        _ => Err(AppError::Invalid(format!("outil inconnu : {tool}"))),
    }
}

/// Échappement pour un littéral de chaîne AppleScript.
fn applescript_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// Script Apple Mail : composition d'un message sortant, envoyé (`send`) ou
/// enregistré en brouillon (`save`). Le premier usage déclenche la demande
/// d'autorisation Automation de macOS (une seule fois).
fn apple_mail_script(to: &str, subject: &str, body: &str, send: bool) -> String {
    format!(
        "tell application \"Mail\"\n\
         set m to make new outgoing message with properties {{subject:\"{}\", content:\"{}\", visible:false}}\n\
         tell m to make new to recipient at end of to recipients with properties {{address:\"{}\"}}\n\
         {} m\n\
         end tell",
        applescript_str(subject),
        applescript_str(body),
        applescript_str(to),
        if send { "send" } else { "save" }
    )
}

fn run_osascript(script: &str) -> Result<String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| AppError::Other(format!("osascript : {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(AppError::Other(format!(
            "Apple Mail a refusé l'opération : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

pub fn parse_iso(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .and_then(|dt| dt.and_local_timezone(chrono::Local).single())
            .map(|dt| dt.timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return dt
            .and_local_timezone(chrono::Local)
            .single()
            .map(|dt| dt.timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return dt
            .and_local_timezone(chrono::Local)
            .single()
            .map(|dt| dt.timestamp());
    }
    None
}

fn photos_search(db: &Db, query: &str, from: Option<&str>, to: Option<&str>) -> Result<Vec<Value>> {
    let kws: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect();
    let from_ts = from.and_then(parse_iso);
    let to_ts = to.and_then(parse_iso);
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, title, path, body, mtime FROM items
             WHERE type='photo' AND status='active' ORDER BY mtime DESC LIMIT 2000",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            let (id, title, path, body, mtime) = r?;
            if let (Some(f), Some(m)) = (from_ts, mtime) {
                if m < f {
                    continue;
                }
            }
            if let (Some(t), Some(m)) = (to_ts, mtime) {
                if m > t {
                    continue;
                }
            }
            let haystack = format!(
                "{} {} {}",
                title.clone().unwrap_or_default(),
                path.clone().unwrap_or_default(),
                body.clone().unwrap_or_default()
            )
            .to_lowercase();
            let hits = kws.iter().filter(|k| haystack.contains(*k)).count();
            if kws.is_empty() || hits > 0 {
                out.push(json!({
                    "id": id, "title": title, "path": path, "exif": body, "mtime": mtime, "hits": hits
                }));
            }
            if out.len() >= 12 {
                break;
            }
        }
        Ok(out)
    })
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;
    use crate::router::intent::Kind;

    /// Chaque intention doit recevoir de quoi agir, et rien de superflu : le
    /// catalogue complet coûtait 18,7 s par itération contre 1,15 s sans outil.
    #[test]
    fn le_catalogue_est_restreint_a_lintention() {
        let complet = catalog().len();
        for (kind, indispensable) in [
            (Kind::MailCompose, "mail.send"),
            (Kind::DocumentCreate, "document.create"),
            (Kind::DeviceDiagnostic, "system.diagnose"),
            (Kind::FileSearch, "files.search"),
            (Kind::Conversation, "memory.query"),
        ] {
            let restreint = catalog_for(kind);
            assert!(
                restreint.iter().any(|spec| spec.name == indispensable),
                "{indispensable} manque pour {kind:?}"
            );
            assert!(restreint.len() < complet, "aucun allègement pour {kind:?}");
        }
        // Rédiger un mail ne doit pas exposer le rangement de fichiers : moins
        // d'outils, c'est aussi moins d'occasions de se tromper d'outil.
        assert!(catalog_for(Kind::MailCompose)
            .iter()
            .all(|spec| spec.name != "files.reorganize"));
    }

    /// Toute entrée de la table doit exister : une faute de frappe retirerait
    /// silencieusement une capacité.
    #[test]
    fn aucun_outil_fantome_dans_la_table_des_intentions() {
        let connus: Vec<String> = catalog().into_iter().map(|spec| spec.name).collect();
        for kind in [
            Kind::MailCompose,
            Kind::DocumentCreate,
            Kind::DeviceDiagnostic,
            Kind::FileSearch,
            Kind::Conversation,
        ] {
            for spec in catalog_for(kind) {
                assert!(connus.contains(&spec.name), "{} inconnu", spec.name);
            }
            assert!(!catalog_for(kind).is_empty(), "{kind:?} sans aucun outil");
        }
    }
}

#[cfg(test)]
mod compte_rendu_tests {
    use super::*;

    /// Cas réel du 18/08 : après confirmation, l'interface affichait
    /// `{"status":"envoyé","to":…,"via":"google"}` dans la conversation.
    /// Un résultat d'outil ne doit jamais atteindre l'utilisateur tel quel.
    #[test]
    fn un_resultat_doutil_devient_une_phrase() {
        let envoi =
            json!({"status":"envoyé","subject":"Bonjour","to":"paul@exemple.fr","via":"google"});
        let phrase = outcome_summary("mail.send", &envoi, false);
        assert_eq!(
            phrase,
            "C'est fait, le mail a été correctement envoyé. Tu peux le retrouver dans tes éléments envoyés sur Gmail."
        );
        assert!(!phrase.contains('{'), "{phrase}");

        // La forme d'adresse choisie par l'utilisateur vaut aussi pour les
        // phrases écrites en dur.
        let vouvoye = outcome_summary("mail.send", &envoi, true);
        assert!(
            vouvoye.contains("Vous pouvez le retrouver dans vos éléments"),
            "{vouvoye}"
        );

        // Un outil sans rendu dédié reste muet sur sa mécanique.
        let inconnu = outcome_summary(
            "un.outil.futur",
            &json!({"status":"ok","payload":{"a":1}}),
            false,
        );
        assert_eq!(inconnu, "Action effectuée.");

        // Un compte rendu déjà rédigé par l'outil prime.
        let range = outcome_summary("files.move", &json!({"report":"3 fichiers rangés."}), false);
        assert_eq!(range, "3 fichiers rangés.");
    }
}
