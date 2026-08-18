//! Retouche des documents Google : Docs, Sheets, Slides.
//!
//! Contrairement à OOXML, où il faut ouvrir l'archive et manipuler le XML,
//! Google expose des opérations STRUCTURÉES (`batchUpdate`). C'est plus fiable :
//! on décrit ce qu'on veut, le fournisseur préserve le reste. On garde donc le
//! même vocabulaire d'opérations que pour Word — l'utilisateur ne doit pas avoir
//! à savoir où vit son document pour demander la même chose.
//!
//! Les fonctions de CONSTRUCTION des requêtes sont pures et testées ; seul
//! l'envoi dépend du réseau.

use crate::error::{AppError, Result};
use crate::tools::docx_edit::{Formatting, Operation, Target};
use serde_json::{json, Value};

/// Ce qu'une retouche a fait, dit dans les mêmes termes que pour Word.
#[derive(Debug, Clone, Default)]
pub struct GoogleEditReport {
    pub touched: usize,
    pub replacements: usize,
    pub added: usize,
    pub placeholders: usize,
}

/// La famille d'un fichier Google, telle que Drive la déclare.
pub fn family_of(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.google-apps.document" => Some("document"),
        "application/vnd.google-apps.spreadsheet" => Some("tableur"),
        "application/vnd.google-apps.presentation" => Some("presentation"),
        _ => None,
    }
}

/// Une couleur hexadécimale vers le triplet 0..1 attendu par les API Google.
pub fn rgb(hex: &str) -> Value {
    let clean = hex.trim().trim_start_matches('#');
    let channel = |start: usize| {
        u8::from_str_radix(clean.get(start..start + 2).unwrap_or("00"), 16).unwrap_or(0) as f64
            / 255.0
    };
    json!({"red": channel(0), "green": channel(2), "blue": channel(4)})
}

/// Le style de texte et la liste des champs modifiés — Google exige les deux,
/// et n'écrase QUE les champs nommés : ce qu'on ne mentionne pas est préservé.
pub fn text_style(formatting: &Formatting) -> (Value, String) {
    let mut style = json!({});
    let mut fields: Vec<&str> = Vec::new();
    if let Some(color) = &formatting.color {
        style["foregroundColor"] = json!({"color": {"rgbColor": rgb(color)}});
        fields.push("foregroundColor");
    }
    if let Some(bold) = formatting.bold {
        style["bold"] = json!(bold);
        fields.push("bold");
    }
    if let Some(italic) = formatting.italic {
        style["italic"] = json!(italic);
        fields.push("italic");
    }
    if let Some(size) = formatting.size_pt {
        style["fontSize"] = json!({"magnitude": size, "unit": "PT"});
        fields.push("fontSize");
    }
    (style, fields.join(","))
}

/// Un paragraphe de Google Docs vu comme Syn en a besoin : son style nommé, son
/// texte, et l'intervalle d'indices sur lequel agir.
#[derive(Debug, Clone, PartialEq)]
pub struct DocParagraph {
    pub style: String,
    pub text: String,
    pub start: i64,
    pub end: i64,
}

/// Lit la structure d'un document Docs. Le corps est une suite d'éléments dont
/// seuls les paragraphes nous intéressent.
pub fn doc_paragraphs(document: &Value) -> Vec<DocParagraph> {
    document["body"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|element| {
            let paragraph = element.get("paragraph")?;
            let style = paragraph["paragraphStyle"]["namedStyleType"]
                .as_str()
                .unwrap_or("NORMAL_TEXT")
                .to_string();
            let runs = paragraph["elements"].as_array()?;
            let mut text = String::new();
            let mut start = i64::MAX;
            let mut end = 0i64;
            for run in runs {
                if let Some(content) = run["textRun"]["content"].as_str() {
                    text.push_str(content);
                    start = start.min(run["startIndex"].as_i64().unwrap_or(0));
                    end = end.max(run["endIndex"].as_i64().unwrap_or(0));
                }
            }
            // Un paragraphe vide n'a pas d'intervalle utile : le mettre en forme
            // ferait échouer tout le lot.
            (start != i64::MAX && end > start).then_some(DocParagraph {
                style,
                text: text.trim_end_matches('\n').to_string(),
                start,
                // Le saut de ligne final n'appartient pas au texte visible :
                // l'inclure colorerait la marque de paragraphe.
                end: end - 1,
            })
        })
        .collect()
}

/// Un style nommé de Docs est-il un titre ? `HEADING_1`… et `TITLE`.
pub fn is_heading_style(style: &str) -> bool {
    style.starts_with("HEADING") || style == "TITLE" || style == "SUBTITLE"
}

/// Traduit les opérations de Syn en requêtes `batchUpdate` pour Docs.
///
/// Les insertions sont placées en fin de document et sont donc construites
/// APRÈS les mises en forme : ajouter du texte décalerait les indices des
/// paragraphes existants et la mise en forme tomberait à côté.
pub fn doc_requests(
    document: &Value,
    operations: &[Operation],
    report: &mut GoogleEditReport,
) -> Vec<Value> {
    let paragraphs = doc_paragraphs(document);
    let mut requests = Vec::new();

    for operation in operations {
        match operation {
            Operation::Format { target, formatting } => {
                let (style, fields) = text_style(formatting);
                if fields.is_empty() {
                    continue;
                }
                for paragraph in &paragraphs {
                    let visé = match target {
                        Target::Headings => is_heading_style(&paragraph.style),
                        Target::Body => !is_heading_style(&paragraph.style),
                        Target::All => true,
                        Target::Containing(needle) => {
                            crate::db::fold(&paragraph.text).contains(&crate::db::fold(needle))
                        }
                    };
                    if !visé {
                        continue;
                    }
                    requests.push(json!({"updateTextStyle": {
                        "range": {"startIndex": paragraph.start, "endIndex": paragraph.end},
                        "textStyle": style,
                        "fields": fields,
                    }}));
                    report.touched += 1;
                }
            }
            Operation::Replace { from, to } if !from.is_empty() => {
                requests.push(json!({"replaceAllText": {
                    "containsText": {"text": from, "matchCase": true},
                    "replaceText": to,
                }}));
                report.replacements += 1;
            }
            _ => {}
        }
    }

    // Les ajouts viennent en dernier, à la fin du corps.
    let fin = document["body"]["content"]
        .as_array()
        .and_then(|content| content.last())
        .and_then(|element| element["endIndex"].as_i64())
        .map(|index| (index - 1).max(1))
        .unwrap_or(1);
    let mut texte_ajoute = String::new();
    for operation in operations {
        match operation {
            Operation::Append { text, heading } => {
                texte_ajoute.push_str(&format!("\n{text}"));
                if *heading {
                    // Le style s'applique au paragraphe une fois le texte inséré.
                    requests.push(json!({"insertText": {
                        "location": {"index": fin},
                        "text": format!("\n{text}"),
                    }}));
                    requests.push(json!({"updateParagraphStyle": {
                        "range": {"startIndex": fin + 1, "endIndex": fin + 1 + text.chars().count() as i64},
                        "paragraphStyle": {"namedStyleType": "HEADING_1"},
                        "fields": "namedStyleType",
                    }}));
                } else {
                    requests.push(json!({"insertText": {
                        "location": {"index": fin},
                        "text": format!("\n{text}"),
                    }}));
                }
                report.added += 1;
            }
            Operation::ImagePlaceholder { description } => {
                requests.push(json!({"insertText": {
                    "location": {"index": fin},
                    "text": format!("\n[ Emplacement d'image — {description} ] Syn ne crée pas d'images : déposez la vôtre ici."),
                }}));
                report.placeholders += 1;
            }
            _ => {}
        }
    }
    requests
}

/// Traduit les opérations pour Slides. Les diapositives n'ont pas de « titres »
/// au sens de Docs : on cible les formes de type TITLE déclarées par le gabarit.
pub fn slides_requests(
    presentation: &Value,
    operations: &[Operation],
    report: &mut GoogleEditReport,
) -> Vec<Value> {
    let mut requests = Vec::new();
    let formes: Vec<(String, String, bool)> = presentation["slides"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|slide| {
            slide["pageElements"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|element| {
            let id = element["objectId"].as_str()?.to_string();
            let shape = element.get("shape")?;
            let texte = shape["text"]["textElements"]
                .as_array()
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run["textRun"]["content"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default();
            let titre = shape["placeholder"]["type"]
                .as_str()
                .is_some_and(|kind| kind == "TITLE" || kind == "CENTERED_TITLE");
            Some((id, texte, titre))
        })
        .collect();

    for operation in operations {
        match operation {
            Operation::Format { target, formatting } => {
                let (style, fields) = text_style(formatting);
                if fields.is_empty() {
                    continue;
                }
                for (id, texte, titre) in &formes {
                    let visé = match target {
                        Target::Headings => *titre,
                        Target::Body => !*titre,
                        Target::All => true,
                        Target::Containing(needle) => {
                            crate::db::fold(texte).contains(&crate::db::fold(needle))
                        }
                    };
                    if !visé || texte.trim().is_empty() {
                        continue;
                    }
                    requests.push(json!({"updateTextStyle": {
                        "objectId": id,
                        "textRange": {"type": "ALL"},
                        "style": style,
                        "fields": fields,
                    }}));
                    report.touched += 1;
                }
            }
            Operation::Replace { from, to } if !from.is_empty() => {
                requests.push(json!({"replaceAllText": {
                    "containsText": {"text": from, "matchCase": true},
                    "replaceText": to,
                }}));
                report.replacements += 1;
            }
            // Ajouter du texte à une présentation suppose de choisir une forme :
            // on complète la première zone de corps de la dernière diapositive.
            Operation::Append { text, .. } => {
                if let Some((id, _, _)) = formes.iter().rev().find(|(_, _, titre)| !titre) {
                    requests.push(json!({"insertText": {
                        "objectId": id,
                        "text": format!("\n{text}"),
                        "insertionIndex": 0,
                    }}));
                    report.added += 1;
                }
            }
            Operation::ImagePlaceholder { description } => {
                if let Some((id, _, _)) = formes.iter().rev().find(|(_, _, titre)| !titre) {
                    requests.push(json!({"insertText": {
                        "objectId": id,
                        "text": format!("\n[ Emplacement d'image — {description} ] Syn ne crée pas d'images."),
                        "insertionIndex": 0,
                    }}));
                    report.placeholders += 1;
                }
            }
            _ => {}
        }
    }
    requests
}

// ————————————————— Envoi —————————————————

async fn google_json(url: &str, token: &str) -> Result<Value> {
    let client = reqwest::Client::new();
    let response = client.get(url).bearer_auth(token).send().await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        return Err(refus(status, &value));
    }
    Ok(value)
}

fn refus(status: reqwest::StatusCode, value: &Value) -> AppError {
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
        return AppError::Security(
            "Ce compte Google n'autorise pas encore Syn à modifier ce document. Reconnecte-le depuis Connecteurs pour accorder cette permission.".into(),
        );
    }
    AppError::Other(format!("Google a refusé la modification : {value}"))
}

async fn batch_update(url: &str, token: &str, body: Value) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let value: Value = response.json().await.unwrap_or(Value::Null);
        return Err(refus(status, &value));
    }
    Ok(())
}

/// Retouche un fichier Google, quel que soit son type.
pub async fn edit(file_id: &str, mime: &str, operations: &[Operation]) -> Result<(String, bool)> {
    let token = super::oauth::access_token("google").await?;
    let mut report = GoogleEditReport::default();
    let famille = family_of(mime).ok_or_else(|| {
        AppError::Invalid("Ce fichier Google n'est pas modifiable par Syn.".into())
    })?;

    match famille {
        "document" => {
            let document = google_json(
                &format!("https://docs.googleapis.com/v1/documents/{file_id}"),
                &token,
            )
            .await?;
            let requests = doc_requests(&document, operations, &mut report);
            if requests.is_empty() {
                return Ok((String::new(), false));
            }
            batch_update(
                &format!("https://docs.googleapis.com/v1/documents/{file_id}:batchUpdate"),
                &token,
                json!({ "requests": requests }),
            )
            .await?;
        }
        "presentation" => {
            let presentation = google_json(
                &format!("https://slides.googleapis.com/v1/presentations/{file_id}"),
                &token,
            )
            .await?;
            let requests = slides_requests(&presentation, operations, &mut report);
            if requests.is_empty() {
                return Ok((String::new(), false));
            }
            batch_update(
                &format!("https://slides.googleapis.com/v1/presentations/{file_id}:batchUpdate"),
                &token,
                json!({ "requests": requests }),
            )
            .await?;
        }
        _ => {
            // Sheets : seuls le remplacement et l'ajout de ligne ont un sens ;
            // la mise en forme de cellules demande de viser une plage, que
            // l'utilisateur devra nommer.
            let mut fait = false;
            for operation in operations {
                match operation {
                    Operation::Replace { from, to } if !from.is_empty() => {
                        batch_update(
                            &format!("https://sheets.googleapis.com/v4/spreadsheets/{file_id}:batchUpdate"),
                            &token,
                            json!({"requests": [{"findReplace": {
                                "find": from, "replacement": to, "allSheets": true, "matchCase": true
                            }}]}),
                        )
                        .await?;
                        report.replacements += 1;
                        fait = true;
                    }
                    Operation::Append { text, .. } => {
                        let cellules: Vec<Value> =
                            text.split([';', '\t']).map(|part| json!(part.trim())).collect();
                        batch_update(
                            &format!("https://sheets.googleapis.com/v4/spreadsheets/{file_id}/values/A1:append?valueInputOption=USER_ENTERED"),
                            &token,
                            json!({"values": [cellules]}),
                        )
                        .await?;
                        report.added += 1;
                        fait = true;
                    }
                    Operation::Format { .. } | Operation::ImagePlaceholder { .. } => {
                        return Err(AppError::Invalid(
                            "Dans un tableur Google, je sais remplacer une valeur et ajouter une ligne, mais pas encore mettre en forme des cellules.".into(),
                        ))
                    }
                    _ => {}
                }
            }
            if !fait {
                return Ok((String::new(), false));
            }
        }
    }

    let mut faits: Vec<String> = Vec::new();
    if report.touched > 0 {
        faits.push(format!("{} passage(s) mis en forme", report.touched));
    }
    if report.replacements > 0 {
        faits.push(format!("{} remplacement(s)", report.replacements));
    }
    if report.added > 0 {
        faits.push(format!("{} ajout(s)", report.added));
    }
    if report.placeholders > 0 {
        faits.push(format!(
            "{} emplacement(s) d'image réservé(s) — Syn ne crée pas d'images",
            report.placeholders
        ));
    }
    Ok((faits.join(", "), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_test() -> Value {
        json!({"body": {"content": [
            {"endIndex": 1},
            {"startIndex": 1, "endIndex": 12, "paragraph": {
                "paragraphStyle": {"namedStyleType": "HEADING_1"},
                "elements": [{"startIndex": 1, "endIndex": 12, "textRun": {"content": "Le titre\n"}}]
            }},
            {"startIndex": 12, "endIndex": 40, "paragraph": {
                "paragraphStyle": {"namedStyleType": "NORMAL_TEXT"},
                "elements": [{"startIndex": 12, "endIndex": 40, "textRun": {"content": "Un paragraphe de corps.\n"}}]
            }}
        ]}})
    }

    /// Le cas de Paul, côté Google : seuls les titres changent de couleur, et
    /// seul le champ « couleur » est écrasé — le reste du style survit.
    #[test]
    fn seuls_les_titres_sont_vises_dans_un_google_doc() {
        let mut report = GoogleEditReport::default();
        let requests = doc_requests(
            &document_test(),
            &[Operation::Format {
                target: Target::Headings,
                formatting: Formatting {
                    color: Some("0000FF".into()),
                    ..Default::default()
                },
            }],
            &mut report,
        );
        assert_eq!(report.touched, 1, "{requests:?}");
        let style = &requests[0]["updateTextStyle"];
        assert_eq!(style["fields"], "foregroundColor");
        assert_eq!(style["range"]["startIndex"], 1);
        // Bleu pur : 0, 0, 1.
        let couleur = &style["textStyle"]["foregroundColor"]["color"]["rgbColor"];
        assert_eq!(couleur["blue"], 1.0);
        assert_eq!(couleur["red"], 0.0);
    }

    #[test]
    fn la_marque_de_paragraphe_reste_hors_de_la_mise_en_forme() {
        let paragraphes = doc_paragraphs(&document_test());
        assert_eq!(paragraphes.len(), 2);
        assert_eq!(paragraphes[0].text, "Le titre");
        // L'intervalle s'arrête avant le saut de ligne final.
        assert_eq!(paragraphes[0].end, 11);
    }

    #[test]
    fn seuls_les_champs_demandes_sont_ecrases() {
        let (style, fields) = text_style(&Formatting {
            bold: Some(true),
            ..Default::default()
        });
        assert_eq!(fields, "bold");
        assert_eq!(style["bold"], true);
        assert!(style.get("foregroundColor").is_none());
    }

    #[test]
    fn une_couleur_hexadecimale_devient_un_triplet_google() {
        let couleur = rgb("#FF8000");
        assert_eq!(couleur["red"], 1.0);
        assert!((couleur["green"].as_f64().unwrap() - 0.502).abs() < 0.01);
        assert_eq!(couleur["blue"], 0.0);
    }

    #[test]
    fn dans_une_presentation_le_titre_est_une_forme_de_gabarit() {
        let presentation = json!({"slides": [{"pageElements": [
            {"objectId": "t1", "shape": {"placeholder": {"type": "TITLE"},
                "text": {"textElements": [{"textRun": {"content": "Ordre du jour"}}]}}},
            {"objectId": "c1", "shape": {
                "text": {"textElements": [{"textRun": {"content": "Trois points"}}]}}}
        ]}]});
        let mut report = GoogleEditReport::default();
        let requests = slides_requests(
            &presentation,
            &[Operation::Format {
                target: Target::Headings,
                formatting: Formatting {
                    bold: Some(true),
                    ..Default::default()
                },
            }],
            &mut report,
        );
        assert_eq!(report.touched, 1);
        assert_eq!(requests[0]["updateTextStyle"]["objectId"], "t1");
    }
}
