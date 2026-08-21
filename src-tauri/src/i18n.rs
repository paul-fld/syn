//! La langue dans laquelle Syn répond.
//!
//! Deux choses distinctes, qu'il ne faut pas confondre :
//!
//! * la langue de TRAVAIL de Syn est l'anglais — consignes internes, mots de
//!   recherche envoyés aux services, raisonnement du modèle. C'est la langue que
//!   les modèles suivent le mieux et celle que parlent Gmail, Graph ou Drive ;
//! * la langue de RÉPONSE est celle de l'utilisateur. Elle est détectée sur ses
//!   propres phrases, jamais imposée.
//!
//! La détection ne connaît que des mots grammaticaux — articles, pronoms,
//! auxiliaires : des classes FERMÉES, donc énumérables sans arbitraire, comme
//! les mots de formulation du chemin de recherche. Elle ne regarde jamais le
//! vocabulaire du sujet, qui peut être dans n'importe quelle langue (« retrouve
//! le mail de Liverpool » reste du français).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Fr,
    En,
}

impl Lang {
    pub fn code(&self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        match code {
            "fr" => Some(Lang::Fr),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Mots grammaticaux propres au français. Aucun n'existe en anglais courant.
const MARQUEURS_FR: &[&str] = &[
    "le", "la", "les", "un", "une", "des", "de", "du", "au", "aux", "je", "tu", "il", "elle",
    "nous",
    "vous", "ils", "elles", "mon", "ma", "mes", "ton", "ta", "tes", "son", "sa", "ses", "notre",
    "votre", "leur", "ce", "cette", "ces", "qui", "que", "quoi", "est", "sont", "etait", "avec",
    "pour", "dans", "sur", "sans", "chez", "vers", "peux", "peut", "veux", "veut", "fais", "fait",
    "moi", "toi", "lui", "pas", "plus", "tres", "mais", "donc", "alors", "quand", "comment",
    "pourquoi", "ou", "et", "aussi", "encore", "deja", "hier", "aujourd", "demain", "merci",
];

/// Mots grammaticaux propres à l'anglais.
const MARQUEURS_EN: &[&str] = &[
    "the", "a", "an", "of", "to", "in", "on", "at", "for", "with", "from", "by", "is", "are",
    "was", "were", "be", "been", "do", "does", "did", "can", "could", "would", "should", "will",
    "my", "your", "his", "her", "its", "our", "their", "this", "that", "these", "those", "what",
    "which", "who", "where", "when", "why", "how", "and", "or", "but", "not", "you", "me", "him",
    "them", "please", "thanks", "about", "have", "has", "had", "get", "got", "need", "want",
    "there", "here", "again", "yesterday", "today", "tomorrow",
];

/// Détecte la langue d'un message. Rend `None` quand rien ne tranche —
/// « gmail », « ok », « le deuxième » : trop court, ou sans marqueur.
///
/// Ne jamais deviner est ici essentiel : un mot isolé ne doit pas faire basculer
/// toute une conversation dans une autre langue.
pub fn detect(text: &str) -> Option<Lang> {
    let plie = crate::db::fold(text);
    let mut fr = 0usize;
    let mut en = 0usize;
    for mot in plie.split(|c: char| !c.is_alphanumeric()) {
        if mot.is_empty() {
            continue;
        }
        // Un mot présent dans les deux langues (« a », « on », « or »…) ne
        // départage rien : on ne le compte pour personne.
        let est_fr = MARQUEURS_FR.contains(&mot);
        let est_en = MARQUEURS_EN.contains(&mot);
        match (est_fr, est_en) {
            (true, false) => fr += 1,
            (false, true) => en += 1,
            _ => {}
        }
    }
    // Deux façons de trancher, et deux seulement : une majorité de deux
    // marqueurs — qui empêche un emprunt isolé (« please », « today ») de
    // renverser une phrase entière — ou l'absence totale de marqueurs adverses,
    // qui suffit pour une demande courte (« retrouve le devis »).
    let franc = |gagnant: usize, perdant: usize| gagnant >= perdant + 2 || (perdant == 0 && gagnant >= 1);
    if fr > en && franc(fr, en) {
        Some(Lang::Fr)
    } else if en > fr && franc(en, fr) {
        Some(Lang::En)
    } else {
        None
    }
}

/// La langue dans laquelle répondre à ce tour de conversation.
///
/// Trois sources, dans cet ordre : le réglage explicite de l'utilisateur, la
/// langue de sa phrase, puis celle de la conversation en cours. La dernière
/// évite qu'un « ok » ou un « gmail » — qui n'ont pas de langue — ne fasse
/// basculer une conversation entière.
pub fn resolve(db: &crate::db::Db, session_id: &str, user_text: &str, reglage: &str) -> Lang {
    if let Some(impose) = Lang::from_code(reglage) {
        return impose;
    }
    if let Some(detectee) = detect(user_text) {
        remember(db, session_id, detectee);
        return detectee;
    }
    session_lang(db, session_id).unwrap_or(Lang::Fr)
}

/// La langue de Syn hors conversation — briefs, réflexes, notifications.
///
/// Ces textes ne répondent à aucune phrase : ils prennent le réglage explicite
/// s'il existe, sinon la langue de la dernière conversation, qui est la
/// meilleure trace de celle que l'utilisateur parle.
pub fn ambient(db: &crate::db::Db, reglage: &str) -> Lang {
    if let Some(impose) = Lang::from_code(reglage) {
        return impose;
    }
    db.read(|c| {
        Ok(c.query_row(
            "SELECT lang FROM sessions WHERE lang IS NOT NULL ORDER BY updated_at DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok())
    })
    .ok()
    .flatten()
    .and_then(|code| Lang::from_code(&code))
    .unwrap_or(Lang::Fr)
}

/// Comment Syn s'adresse à l'utilisateur DANS une conversation donnée.
///
/// La langue a déjà été établie au début du tour par [`resolve`] ; on la relit
/// simplement, ce qui évite de la faire voyager à travers toutes les signatures
/// d'un parcours à plusieurs étapes.
pub fn session_speak(
    db: &crate::db::Db,
    session_id: &str,
    settings: &crate::settings::Settings,
) -> Speak {
    let lang = Lang::from_code(&settings.answer_language)
        .or_else(|| session_lang(db, session_id))
        .unwrap_or(Lang::Fr);
    Speak {
        lang,
        formal: settings.voice.vouvoie(),
    }
}

/// Comment Syn s'adresse à l'utilisateur hors conversation.
pub fn ambient_speak(db: &crate::db::Db, settings: &crate::settings::Settings) -> Speak {
    Speak {
        lang: ambient(db, &settings.answer_language),
        formal: settings.voice.vouvoie(),
    }
}

fn session_lang(db: &crate::db::Db, session_id: &str) -> Option<Lang> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT lang FROM sessions WHERE id=?1",
            rusqlite::params![session_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten())
    })
    .ok()
    .flatten()
    .and_then(|code| Lang::from_code(&code))
}

fn remember(db: &crate::db::Db, session_id: &str, lang: Lang) {
    let _ = db.with(|c| {
        c.execute(
            "UPDATE sessions SET lang=?2 WHERE id=?1",
            rusqlite::params![session_id, lang.code()],
        )?;
        Ok(())
    });
}

/// Comment Syn s'adresse à cet utilisateur : sa langue, et son degré de
/// familiarité.
///
/// Ce couple voyage ensemble jusqu'au texte affiché. Le vouvoiement n'existe
/// qu'en français ; en anglais, la même phrase sert aux deux.
#[derive(Debug, Clone, Copy)]
pub struct Speak {
    pub lang: Lang,
    pub formal: bool,
}

impl Speak {
    pub fn fr(formal: bool) -> Self {
        Speak {
            lang: Lang::Fr,
            formal,
        }
    }

    pub fn en() -> Self {
        Speak {
            lang: Lang::En,
            formal: false,
        }
    }

    /// Choisit entre les trois variantes d'une même phrase.
    pub fn pick<'a>(&self, tu: &'a str, vous: &'a str, english: &'a str) -> &'a str {
        match (self.lang, self.formal) {
            (Lang::En, _) => english,
            (Lang::Fr, true) => vous,
            (Lang::Fr, false) => tu,
        }
    }

    /// Variante sans distinction de familiarité (une seule forme française).
    pub fn either<'a>(&self, francais: &'a str, english: &'a str) -> &'a str {
        match self.lang {
            Lang::Fr => francais,
            Lang::En => english,
        }
    }

    pub fn is_en(&self) -> bool {
        self.lang == Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnait_les_deux_langues_sur_des_phrases_ordinaires() {
        assert_eq!(
            detect("Tu peux me retrouver le mail de liverpool pour le match ?"),
            Some(Lang::Fr)
        );
        assert_eq!(
            detect("Can you find the email from Liverpool about my tickets?"),
            Some(Lang::En)
        );
        assert_eq!(
            detect("where is the invoice from the garage"),
            Some(Lang::En)
        );
    }

    /// Le sujet peut être dans une autre langue que la demande : c'est
    /// précisément le cas qui a mis Syn en échec.
    #[test]
    fn le_vocabulaire_du_sujet_ne_change_pas_la_langue_de_la_demande() {
        assert_eq!(
            detect("retrouve le Booking Confirmation de Liverpool Football Club"),
            Some(Lang::Fr)
        );
        assert_eq!(
            detect("open the facture from Nexity please"),
            Some(Lang::En)
        );
    }

    /// Un mot isolé ne fait basculer aucune conversation.
    #[test]
    fn sans_signe_franc_syn_ne_devine_pas() {
        assert_eq!(detect("gmail"), None);
        assert_eq!(detect("ok"), None);
        assert_eq!(detect(""), None);
        assert_eq!(detect("Liverpool 2026"), None);
    }

    #[test]
    fn le_vouvoiement_nexiste_quen_francais() {
        assert_eq!(Speak::fr(true).pick("salut", "bonjour", "hello"), "bonjour");
        assert_eq!(Speak::fr(false).pick("salut", "bonjour", "hello"), "salut");
        assert_eq!(Speak::en().pick("salut", "bonjour", "hello"), "hello");
    }
}
