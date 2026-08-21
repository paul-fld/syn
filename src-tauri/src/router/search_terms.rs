//! Les mots avec lesquels chercher — dans la langue de la demande ET en anglais.
//!
//! Une demande et la chose cherchée ne sont pas toujours dans la même langue :
//! Paul écrit en français à un assistant qui doit retrouver un message rédigé en
//! anglais par un club anglais. Aucune liste de mots ne peut couvrir ça, et
//! aucune règle non plus : traduire est un travail de compréhension.
//!
//! C'est donc le modèle qui le fait — et rien d'autre. Le déterministe garde
//! son rôle : il MESURE ce que valent ces mots dans le corpus réel de
//! l'utilisateur (`retrieval::ranked_terms`), construit les requêtes et classe
//! les résultats. Un mot inventé par le modèle n'a aucun poids particulier : il
//! sera simplement rare, donc traité comme un mot dont on ne sait rien.
//!
//! Deux garde-fous tiennent tout le reste :
//! * cet appel est SÉPARÉ de la classification d'intention. Ajouter des
//!   consignes au prompt de classification avait été mesuré : 4,3 % → 17,4 %
//!   d'erreur. Ce chemin-ci ne peut donc rien lui coûter.
//! * il est facultatif. Hors ligne, trop lent, ou réponse illisible : on garde
//!   les mots extraits déterministiquement, c'est-à-dire le comportement
//!   d'avant.

use crate::llm::{ChatMessage, GenParams, LlmClient};
use std::sync::Arc;

/// Consigne en anglais, délibérément.
///
/// Les modèles ouverts de cette taille suivent mieux une consigne en anglais, et
/// c'est aussi la langue dans laquelle ils traduisent le plus sûrement. La
/// langue de travail interne de Syn n'a pas à être celle de l'utilisateur : ce
/// qu'il voit, lui, reste dans la sienne.
const SYSTEM: &str = r#"You turn a person's request into keywords for searching their own emails, files and documents.

Answer with JSON only: {"terms":["...","..."]}

Rules:
- 2 to 6 single words. Never a sentence, never a phrase.
- Keep proper nouns exactly as written (names of people, companies, clubs, places, products).
- Drop every word that describes the SEARCH itself rather than the thing looked for (find, show, email, message, file, document, folder, please).
- If the request is not in English, ALSO add the English equivalent of each meaningful word — the thing being looked for is often written in another language than the request.
- Add nothing the request does not imply. No synonyms of your own invention, no guessed context.

Examples:
Request: retrouve le mail de liverpool concernant mes tickets pour le match du 2 décembre
{"terms":["liverpool","tickets","ticket","match","december","décembre"]}
Request: where is the Q3 revenue forecast
{"terms":["Q3","revenue","forecast"]}
Request: hol dir die Rechnung von der Autowerkstatt
{"terms":["Rechnung","invoice","Autowerkstatt","garage"]}"#;

/// Longueur au-delà de laquelle un « mot » n'en est plus un : le modèle a
/// recopié une phrase, on ne la cherchera pas.
const MOT_MAX: usize = 24;

/// Demande au modèle les mots de recherche. Rend une liste vide si quoi que ce
/// soit échoue — l'appelant retombe alors sur l'extraction déterministe.
pub async fn from_model(llm: &Arc<dyn LlmClient>, request: &str) -> Vec<String> {
    let request = request.trim();
    if request.is_empty() {
        return vec![];
    }
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: format!("Request: {request}"),
        tool_calls: None,
        tool_name: None,
    }];
    let params = GenParams {
        temperature: 0.0,
        max_tokens: Some(120),
        json: true,
    };
    let Ok(response) = llm.generate(SYSTEM, &messages, &[], params).await else {
        return vec![];
    };
    parse(&response.content)
}

/// Extrait les mots du JSON rendu par le modèle, et les nettoie.
///
/// Tout est vérifié ici : un modèle peut rendre du texte autour du JSON, des
/// phrases entières, des doublons, ou trente mots. Rien de tout cela ne doit
/// atteindre une requête envoyée à un fournisseur.
fn parse(raw: &str) -> Vec<String> {
    let Some(start) = raw.find('{') else {
        return vec![];
    };
    let Some(end) = raw.rfind('}') else {
        return vec![];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw[start..=end]) else {
        return vec![];
    };
    let mut out: Vec<String> = vec![];
    for terme in value["terms"].as_array().cloned().unwrap_or_default() {
        let Some(terme) = terme.as_str() else { continue };
        // Un « mot » en plusieurs morceaux est découpé : chaque morceau vaut
        // pour lui-même dans une recherche conjonctive.
        for morceau in terme.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-') {
            let morceau = morceau.trim_matches(|c: char| !c.is_alphanumeric());
            if morceau.is_empty() || morceau.chars().count() > MOT_MAX {
                continue;
            }
            let chiffre = morceau.chars().any(|c| c.is_ascii_digit());
            if morceau.chars().count() < if chiffre { 2 } else { 3 } {
                continue;
            }
            if crate::retrieval::is_request_filler(&crate::db::fold(morceau)) {
                continue;
            }
            let plie = crate::db::fold(morceau);
            if !out.iter().any(|deja| crate::db::fold(deja) == plie) {
                out.push(morceau.to_string());
            }
            if out.len() == 8 {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_mots_rendus_par_le_modele_sont_nettoyes() {
        let brut = r#"Sure! {"terms":["liverpool","tickets","le match du 2 décembre","email","a","liverpool"]}"#;
        assert_eq!(
            parse(brut),
            vec!["liverpool", "tickets", "match", "décembre"],
            "les phrases sont découpées, « email » nomme le contenant, \
             les doublons et les mots trop courts tombent"
        );
    }

    #[test]
    fn une_reponse_illisible_ne_produit_aucun_mot() {
        assert!(parse("je ne sais pas").is_empty());
        assert!(parse("{\"terms\": \"pas un tableau\"}").is_empty());
        assert!(parse("").is_empty());
    }

    /// Un modèle bavard ne doit pas pouvoir noyer la requête.
    #[test]
    fn le_nombre_de_mots_est_borne() {
        let mots: Vec<String> = (0..40).map(|index| format!("\"mot{index}\"")).collect();
        let brut = format!("{{\"terms\":[{}]}}", mots.join(","));
        assert_eq!(parse(&brut).len(), 8);
    }
}
