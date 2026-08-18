//! Compréhension de l'intention (Intelligence §6.1).
//!
//! Ce module remplace un aiguillage à mots-clés par une compréhension du SENS.
//! La distinction est produit, pas technique : une porte à mots-clés n'ouvre
//! qu'aux utilisateurs qui emploient les mots prévus par le développeur — en
//! pratique, aux francophones qui formulent leurs demandes comme lui. « Où j'ai
//! foutu mon attestation ? », « Where's my lease? » et « mon bail » expriment la
//! même intention et doivent suivre le même chemin.
//!
//! Le modèle local classe ; le déterministe reste en secours strict quand le
//! modèle est indisponible (hors ligne, Ollama arrêté) — jamais l'inverse, pour
//! que la formulation d'un utilisateur ne soit jamais la condition du service.

use crate::error::Result;
use crate::llm::{ChatMessage, GenParams, LlmClient};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Budget de latence de la classification. Au-delà, on répond avec le secours
/// déterministe plutôt que de faire attendre l'utilisateur.
const BUDGET: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Retrouver quelque chose que l'utilisateur possède déjà.
    FileSearch,
    /// Retrouver un message REÇU dans une de ses messageries.
    ///
    /// Distinct de `FileSearch` : un mail ne vit pas dans un dossier, il vit
    /// dans une boîte, et il se retrouve par une recherche chez le fournisseur.
    /// Les confondre envoyait les demandes de mail vers l'index de fichiers,
    /// qui répondait par des documents dont le NOM contenait « mail ».
    MailSearch,
    /// Écrire à une personne.
    MailCompose,
    /// Renseigner sur l'état de la machine.
    DeviceDiagnostic,
    /// Produire un nouveau document.
    DocumentCreate,
    /// Tout le reste : la boucle agentique décide.
    Conversation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Any,
    Local,
    Google,
    Microsoft,
    AnyCloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Compris par le modèle local.
    Understood,
    /// Deviné par les heuristiques de secours (modèle indisponible).
    Fallback,
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub kind: Kind,
    pub scope: Scope,
    /// Ce que l'utilisateur cherche ou veut produire, dans ses propres mots.
    pub subject: Option<String>,
    pub source: Source,
}

impl Intent {
    fn conversation(source: Source) -> Self {
        Self {
            kind: Kind::Conversation,
            scope: Scope::Any,
            subject: None,
            source,
        }
    }
}

/// Consigne de classification : elle décrit les intentions par leur SENS.
///
/// Elle ne contient DÉLIBÉRÉMENT aucune formulation d'exemple. Une
/// première version en donnait ; le modèle les recopiait dans le champ `subject`
/// de demandes sans rapport (« merci, c'est parfait » ressortait avec le sujet
/// d'un exemple). Décrire les intentions par leur critère de décision, et non
/// par des tournures, supprime cette fuite — et garantit qu'aucune manière de
/// parler n'est privilégiée.
const SYSTEM: &str = r#"Tu classes l'intention d'une demande adressée à un assistant personnel.

Réponds UNIQUEMENT par un objet JSON :
{"intent": "...", "scope": "...", "subject": "..."}

Choisis UNE intention, d'après ce que l'utilisateur veut obtenir :

- "file_search" — il veut remettre la main sur une chose qui EXISTE DÉJÀ et qui
  lui appartient : document, fichier, note, présentation, tableur. Le critère
  est la possession, pas le vocabulaire : demander où est une chose, se plaindre
  de ne plus la trouver, en avoir besoin, demander si tu l'as, ou simplement la
  nommer, relèvent tous de cette intention.

- "mail_search" — il veut retrouver un MESSAGE reçu ou envoyé dans une de ses
  messageries. Le critère est un signe EXPLICITE que la chose est arrivée par
  message : le mot mail, message ou courriel ; le fait de l'avoir « reçu » ;
  ou un expéditeur qui la lui a envoyée (entreprise, service, personne). Sans
  un de ces signes, une chose qu'il possède est un document : "file_search".

- "mail_compose" — il veut qu'un message PARTE vers une personne nommée. Le
  critère est la présence d'un destinataire humain et d'un acte de parole qui
  lui est adressé, même si aucun mot de messagerie n'apparaît. Écrire à
  quelqu'un n'est pas retrouver un message reçu : c'est l'opposé.

- "device_diagnostic" — sa demande porte sur l'ordinateur lui-même : vitesse,
  chaleur, bruit, batterie, espace libre, mémoire. Le critère est que la
  réponse se trouve dans l'état de la machine, pas dans ses documents. Une
  plainte de ressenti en fait partie.

- "document_create" — il veut qu'une chose NOUVELLE soit rédigée, qui n'existe
  pas encore. Le critère est la production, opposé exact de "file_search".

- "conversation" — tout le reste : savoir général, avis, remerciement,
  question sur toi. Critère : la réponse ne dépend d'aucune donnée personnelle.

Le critère décisif entre "file_search" et "conversation" est la POSSESSION :
demander ce qu'est une chose est une question de savoir ; demander une chose
que l'on possède est une recherche.

scope — SEULEMENT si l'utilisateur nomme lui-même un service ou un support.
S'il n'en nomme aucun, réponds "any" ; ne devine jamais un emplacement.
Valeurs : "google", "microsoft", "cloud", "local", "any".

subject — la chose visée, recopiée telle que l'utilisateur l'a écrite, sans les
verbes de sa demande ni les noms de services. Chaîne vide s'il n'y en a pas.
N'invente jamais un sujet : s'il n'y en a pas dans la demande, laisse vide.

Si des « Échanges précédents » sont fournis, ils servent UNIQUEMENT à
comprendre la dernière demande. Règle décisive : quand la dernière demande ne
fait que RÉPONDRE à une question que tu viens de poser — un compte, un
emplacement, un choix, un accord — elle garde l'intention du tour précédent.
Un mot isolé n'ouvre jamais une intention nouvelle, même s'il évoque un autre
domaine (« sur le Mac » après « où l'enregistrer ? » reste une création de
document, pas une question sur la machine).
Classe toujours la dernière demande.

Ne traduis rien. Ne commente pas. JSON seul."#;

/// Exemples de calibrage. Ils montrent au modèle OÙ passent les frontières —
/// possession contre savoir, production contre récupération — et non quels mots
/// employer. Ils sont volontairement hétérogènes en langue, en registre et en
/// longueur.
///
/// Invariant de mesure : **aucun de ces exemples ne figure dans le corpus
/// d'évaluation** (`router/eval.rs`). Sans cette disjonction, le taux d'erreur
/// mesurerait la mémoire du prompt, pas la capacité à comprendre une demande
/// jamais vue.
const CALIBRATION: &[(&str, &str)] = &[
    (
        "tu retrouves le devis de la cuisine ?",
        r#"{"intent":"file_search","scope":"any","subject":"devis de la cuisine"}"#,
    ),
    (
        "c'est quoi la différence entre un devis et une facture ?",
        r#"{"intent":"conversation","scope":"any","subject":""}"#,
    ),
    (
        "I can't put my hands on the insurance certificate",
        r#"{"intent":"file_search","scope":"any","subject":"insurance certificate"}"#,
    ),
    (
        "le pacte d'associés traîne dans SharePoint",
        r#"{"intent":"file_search","scope":"microsoft","subject":"pacte d'associés"}"#,
    ),
    // Frontière décisive : la chose cherchée est arrivée dans une boîte mail.
    (
        "retrouve le message d'Air France avec mes billets",
        r#"{"intent":"mail_search","scope":"any","subject":"message d'Air France avec mes billets"}"#,
    ),
    (
        "j'ai reçu un truc de la banque la semaine dernière, tu le retrouves ?",
        r#"{"intent":"mail_search","scope":"any","subject":"un truc de la banque"}"#,
    ),
    (
        "dis à Karim que le rendez-vous est décalé",
        r#"{"intent":"mail_compose","scope":"any","subject":"Karim"}"#,
    ),
    // Acte de parole adressé, sans aucun mot de messagerie : la frontière tient
    // à la présence d'un destinataire humain, pas au mot « mail ».
    (
        "félicite Léa pour sa promotion",
        r#"{"intent":"mail_compose","scope":"any","subject":"Léa"}"#,
    ),
    // « fichier » et « document » apparaissent aussi dans les demandes de
    // création : c'est le verbe de production qui tranche, pas le nom.
    (
        "mets ça au propre dans un document",
        r#"{"intent":"document_create","scope":"any","subject":""}"#,
    ),
    (
        "mon ventilo souffle non-stop",
        r#"{"intent":"device_diagnostic","scope":"any","subject":""}"#,
    ),
    (
        "how much memory is left?",
        r#"{"intent":"device_diagnostic","scope":"any","subject":""}"#,
    ),
    (
        "fais-moi une synthèse écrite de nos échanges",
        r#"{"intent":"document_create","scope":"any","subject":"synthèse de nos échanges"}"#,
    ),
    (
        "write up the release notes as a doc",
        r#"{"intent":"document_create","scope":"any","subject":"release notes"}"#,
    ),
    (
        "super, nickel",
        r#"{"intent":"conversation","scope":"any","subject":""}"#,
    ),
];

fn parse_kind(value: &str) -> Option<Kind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "file_search" => Some(Kind::FileSearch),
        "mail_search" => Some(Kind::MailSearch),
        "mail_compose" => Some(Kind::MailCompose),
        "device_diagnostic" => Some(Kind::DeviceDiagnostic),
        "document_create" => Some(Kind::DocumentCreate),
        "conversation" => Some(Kind::Conversation),
        _ => None,
    }
}

fn parse_scope(value: &str) -> Scope {
    match value.trim().to_ascii_lowercase().as_str() {
        "google" => Scope::Google,
        "microsoft" => Scope::Microsoft,
        "cloud" => Scope::AnyCloud,
        "local" => Scope::Local,
        _ => Scope::Any,
    }
}

/// Le modèle peut encadrer son JSON de texte : on récupère le premier objet.
fn extract_json(content: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(content.trim()) {
        return Some(value);
    }
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    serde_json::from_str(content.get(start..=end)?).ok()
}

/// Fait traiter le prompt de classification par le modèle, sans rien en
/// attendre.
///
/// Charger les POIDS ne suffit pas : la première classification payait encore
/// l'évaluation des ~1 400 jetons de consigne et d'exemples — mesuré à 18 s,
/// bien au-delà du budget de 6 s. La première demande de chaque session partait
/// donc systématiquement au secours déterministe. Une fois le préfixe traité,
/// le modèle le retrouve en cache et répond en 1 à 2 s.
pub async fn preheat(llm: &Arc<dyn LlmClient>) {
    let _ = ask_model(llm, "bonjour", &[]).await;
}

/// Classe la demande. Ne renvoie jamais d'erreur : une compréhension impossible
/// dégrade vers le secours déterministe, elle ne bloque pas l'utilisateur.
/// `context` : les derniers tours de la conversation, du plus ancien au plus
/// récent, sous la forme (rôle, texte).
///
/// Sans ce contexte, une réponse comme « gmail » ou « oui, envoie » n'a aucun
/// sens isolée : Syn la classait au hasard et repartait sur une recherche de
/// documents en plein milieu d'un envoi de mail. Une conversation n'est pas une
/// suite de demandes indépendantes.
pub async fn classify(
    llm: &Arc<dyn LlmClient>,
    text: &str,
    context: &[(String, String)],
    fallback: Intent,
) -> Intent {
    match tokio::time::timeout(BUDGET, ask_model(llm, text, context)).await {
        Ok(Ok(Some(intent))) => intent,
        // Modèle absent, lent, ou réponse inexploitable : on garde le service.
        _ => fallback,
    }
}

/// Met la demande en situation : les échanges précédents d'abord, la phrase à
/// classer ensuite, clairement séparés.
fn situated(text: &str, context: &[(String, String)]) -> String {
    if context.is_empty() {
        return text.to_string();
    }
    let mut prompt = String::from("Échanges précédents :\n");
    for (role, content) in context.iter().rev().take(6).rev() {
        let who = if role == "user" { "utilisateur" } else { "assistant" };
        let extrait: String = content.chars().take(240).collect();
        prompt.push_str(&format!("{who} : {extrait}\n"));
    }
    prompt.push_str("\nDemande à classer :\n");
    prompt.push_str(text);
    prompt
}

async fn ask_model(
    llm: &Arc<dyn LlmClient>,
    text: &str,
    context: &[(String, String)],
) -> Result<Option<Intent>> {
    let mut messages = Vec::with_capacity(CALIBRATION.len() * 2 + 1);
    for (demande, verdict) in CALIBRATION {
        messages.push(ChatMessage::user(*demande));
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: (*verdict).into(),
            tool_calls: None,
            tool_name: None,
        });
    }
    messages.push(ChatMessage::user(situated(text, context)));
    let response = llm
        .generate(
            SYSTEM,
            &messages,
            &[],
            GenParams {
                temperature: 0.0,
                // Le verdict tient en une quarantaine de jetons. Laisser le
                // plafond haut ne change pas la réponse, mais laisse le modèle
                // partir en digression quand il hésite — donc attendre.
                max_tokens: Some(80),
                json: true,
            },
        )
        .await?;
    let Some(value) = extract_json(&response.content) else {
        return Ok(None);
    };
    let Some(kind) = value["intent"].as_str().and_then(parse_kind) else {
        return Ok(None);
    };
    Ok(Some(Intent {
        kind,
        scope: validated_scope(parse_scope(value["scope"].as_str().unwrap_or("any")), text),
        subject: validated_subject(value["subject"].as_str(), text),
        source: Source::Understood,
    }))
}

/// Restreindre la recherche à un service est une décision que seul l'utilisateur
/// peut prendre : s'il n'en nomme aucun, chercher partout est le comportement
/// juste. Le modèle proposait « local » par défaut, ce qui masquait les
/// documents cloud de demandes qui ne parlaient d'aucun emplacement.
///
/// Ce contrôle repose sur des NOMS PROPRES de services — un inventaire fermé et
/// vérifiable — jamais sur une tournure. Ne nommer aucun service n'exclut
/// personne : cela élargit la recherche.
fn validated_scope(proposed: Scope, text: &str) -> Scope {
    if proposed == Scope::Any {
        return Scope::Any;
    }
    let folded = crate::db::fold(text);
    let mentions = |names: &[&str]| names.iter().any(|name| folded.contains(name));
    let named = match proposed {
        // Les noms de produits comptent au même titre que ceux des services :
        // « le Word du budget » nomme bien un écosystème.
        Scope::Google => mentions(&[
            "google", "drive", "gdocs", "gsheet", "gslide", "docs", "sheets", "slides",
        ]),
        Scope::Microsoft => mentions(&[
            "onedrive",
            "sharepoint",
            "microsoft",
            "office 365",
            "microsoft 365",
            "outlook",
            "word",
            "excel",
            "powerpoint",
            "power point",
        ]),
        Scope::AnyCloud => mentions(&["cloud", "en ligne", "online"]),
        Scope::Local => mentions(&[
            "mac", "disque", "disk", "local", "ordinateur", "machine", "hors ligne", "offline",
        ]),
        Scope::Any => true,
    };
    if named {
        proposed
    } else {
        Scope::Any
    }
}

/// Un sujet doit venir de la demande, pas de l'imagination du modèle. On exige
/// qu'au moins un de ses mots porteurs figure réellement dans le texte de
/// l'utilisateur : c'est ce qui a éliminé les sujets recopiés d'un exemple.
fn validated_subject(proposed: Option<&str>, text: &str) -> Option<String> {
    let subject = proposed.map(str::trim).filter(|subject| {
        !subject.is_empty() && subject.chars().count() >= 2 && *subject != "..."
    })?;
    let folded_text = crate::db::fold(text);
    let grounded = crate::db::fold(subject)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .any(|word| folded_text.contains(word));
    grounded.then(|| subject.to_string())
}

/// Secours déterministe. Il n'est PAS le contrat : il tient le service quand le
/// modèle local est arrêté, en acceptant de ne reconnaître que les formulations
/// les plus explicites. Toute erreur ici est une dégradation annoncée, pas le
/// comportement nominal.
pub fn fallback(
    text: &str,
    keyword_file_search: Option<(String, bool)>,
    keyword_scope: Scope,
    keyword_diagnostic: bool,
    keyword_mail: bool,
    keyword_mail_search: bool,
) -> Intent {
    // Chercher un mail reçu passe avant chercher un fichier : « retrouve le
    // mail de… » contient les mots des deux, et l'index de fichiers ne sait
    // pas répondre.
    if keyword_mail_search {
        return Intent {
            kind: Kind::MailSearch,
            scope: keyword_scope,
            subject: keyword_file_search.map(|(subject, _)| subject),
            source: Source::Fallback,
        };
    }
    if keyword_diagnostic {
        return Intent {
            kind: Kind::DeviceDiagnostic,
            scope: Scope::Any,
            subject: None,
            source: Source::Fallback,
        };
    }
    if let Some((subject, _)) = keyword_file_search {
        return Intent {
            kind: Kind::FileSearch,
            scope: keyword_scope,
            subject: Some(subject),
            source: Source::Fallback,
        };
    }
    if keyword_mail {
        return Intent {
            kind: Kind::MailCompose,
            scope: Scope::Any,
            subject: None,
            source: Source::Fallback,
        };
    }
    let _ = text;
    Intent::conversation(Source::Fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_json_du_modele_est_lu_meme_entoure_de_texte() {
        let value =
            extract_json("Voici :\n{\"intent\":\"file_search\",\"scope\":\"google\"}\nVoilà.")
                .unwrap();
        assert_eq!(value["intent"], "file_search");
        assert_eq!(parse_scope(value["scope"].as_str().unwrap()), Scope::Google);
    }

    #[test]
    fn une_intention_inconnue_nest_pas_devinee() {
        assert!(parse_kind("do_something_else").is_none());
        assert_eq!(parse_scope("dropbox"), Scope::Any);
    }
}
