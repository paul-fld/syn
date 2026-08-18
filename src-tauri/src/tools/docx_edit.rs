//! Retouche d'un document Word EXISTANT, en préservant tout le reste.
//!
//! Un docx est une archive OOXML : le texte, sa mise en forme, les styles, les
//! images et les métadonnées y vivent dans des parties séparées. Réécrire le
//! fichier à partir du texte extrait — la seule chose que Syn savait faire —
//! détruirait tout ce qui n'est pas du texte. On ouvre donc l'archive, on ne
//! modifie QUE `word/document.xml`, et on recopie chaque autre partie à
//! l'identique.
//!
//! Le principe qui gouverne ce module : **ne jamais toucher à ce qu'on n'a pas
//! compris**. Une balise inconnue est recopiée telle quelle. Une opération qui
//! ne trouve pas sa cible ne fait rien et le DIT, plutôt que d'approximer.

use crate::error::{AppError, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};
use serde::Serialize;
use std::io::{Cursor, Read, Write};
use std::path::Path;

/// Ce qu'une retouche a réellement fait, pour le dire à l'utilisateur sans
/// l'inventer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EditReport {
    pub paragraphs_touched: usize,
    pub replacements: usize,
    pub paragraphs_added: usize,
    pub placeholders_added: usize,
}

impl EditReport {
    pub fn is_empty(&self) -> bool {
        self.paragraphs_touched == 0
            && self.replacements == 0
            && self.paragraphs_added == 0
            && self.placeholders_added == 0
    }
}

/// Sur quels paragraphes une retouche s'applique.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Les titres : paragraphes dont le style commence par « Titre »/« Heading ».
    Headings,
    /// Le corps : tout ce qui n'est pas un titre.
    Body,
    /// Tous les paragraphes.
    All,
    /// Ceux qui contiennent ce texte.
    Containing(String),
}

impl Target {
    fn matches(&self, style: &str, text: &str) -> bool {
        match self {
            Target::Headings => is_heading(style),
            Target::Body => !is_heading(style),
            Target::All => true,
            Target::Containing(needle) => crate::db::fold(text).contains(&crate::db::fold(needle)),
        }
    }
}

/// Un style de titre, quelle que soit la langue de Word : `Heading1`,
/// `Titre1`, `berschrift1`… Le préfixe suffit et évite d'énumérer les langues.
fn is_heading(style: &str) -> bool {
    let folded = crate::db::fold(style);
    folded.starts_with("heading")
        || folded.starts_with("titre")
        || folded.starts_with("title")
        || folded.contains("berschrift")
}

/// Une mise en forme demandée. Chaque champ absent est laissé tel quel : on ne
/// remet jamais à zéro ce que l'utilisateur n'a pas mentionné.
#[derive(Debug, Clone, Default)]
pub struct Formatting {
    /// Couleur hexadécimale sans dièse, ex. « 0000FF ».
    pub color: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    /// Taille en points (Word la stocke en demi-points).
    pub size_pt: Option<u32>,
}

impl Formatting {
    fn is_empty(&self) -> bool {
        self.color.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.size_pt.is_none()
    }
}

/// Une opération de retouche, décrite en termes de document et non de XML.
#[derive(Debug, Clone)]
pub enum Operation {
    /// Mettre en forme les paragraphes visés.
    Format {
        target: Target,
        formatting: Formatting,
    },
    /// Remplacer un texte par un autre, partout où il apparaît.
    Replace { from: String, to: String },
    /// Ajouter un paragraphe à la fin du document.
    Append { text: String, heading: bool },
    /// Réserver la place d'une image que Syn ne sait pas produire.
    ImagePlaceholder { description: String },
}

/// Applique des opérations à un docx et rend le nouveau contenu du fichier.
///
/// Le fichier n'est pas écrit ici : l'appelant décide quand, et garde une copie
/// de sauvegarde. Rien de destructeur ne se produit sans confirmation.
pub fn apply(path: &Path, operations: &[Operation]) -> Result<(Vec<u8>, EditReport)> {
    let file = std::fs::File::open(path)
        .map_err(|error| AppError::NotFound(format!("Document illisible : {error}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::Invalid("Ce fichier n'est pas un document Word valide.".into()))?;

    let mut parts: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| AppError::Other(error.to_string()))?;
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::Other(error.to_string()))?;
        parts.push((name, bytes));
    }

    let mut report = EditReport::default();
    let document = parts
        .iter_mut()
        .find(|(name, _)| name == "word/document.xml")
        .ok_or_else(|| {
            AppError::Invalid("Ce document Word ne contient pas de partie principale.".into())
        })?;
    let xml = String::from_utf8_lossy(&document.1).into_owned();
    let edited = transform(&xml, operations, &mut report)?;
    document.1 = edited.into_bytes();

    // Réécriture de l'archive : toutes les parties, dans l'ordre, celles qu'on
    // n'a pas touchées comprises.
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in &parts {
            writer
                .start_file(name, options)
                .map_err(|error| AppError::Other(error.to_string()))?;
            writer
                .write_all(bytes)
                .map_err(|error| AppError::Other(error.to_string()))?;
        }
        writer
            .finish()
            .map_err(|error| AppError::Other(error.to_string()))?;
    }
    Ok((buffer.into_inner(), report))
}

/// Le paragraphe courant, pendant la traversée du XML.
#[derive(Default)]
struct Paragraph {
    events: Vec<Event<'static>>,
    style: String,
    text: String,
}

/// Traversée unique du document : on accumule paragraphe par paragraphe, on
/// décide à sa fermeture, et tout ce qu'on ne modifie pas ressort intact.
fn transform(xml: &str, operations: &[Operation], report: &mut EditReport) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut current: Option<Paragraph> = None;
    let mut depth = 0usize;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| AppError::Other(format!("document Word illisible : {error}")))?;
        match &event {
            Event::Eof => break,
            Event::Start(start) if start.name().as_ref() == b"w:p" => {
                depth += 1;
                if depth == 1 {
                    current = Some(Paragraph::default());
                }
            }
            Event::End(end) if end.name().as_ref() == b"w:p" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(paragraph) = current.take() {
                        write_paragraph(&mut writer, paragraph, operations, report)?;
                        continue;
                    }
                }
            }
            _ => {}
        }
        match &mut current {
            Some(paragraph) => {
                match &event {
                    Event::Start(start) if start.name().as_ref() == b"w:pStyle" => {
                        if let Some(value) = attribute(start, b"w:val") {
                            paragraph.style = value;
                        }
                    }
                    Event::Empty(start) if start.name().as_ref() == b"w:pStyle" => {
                        if let Some(value) = attribute(start, b"w:val") {
                            paragraph.style = value;
                        }
                    }
                    Event::Text(text) => {
                        paragraph
                            .text
                            .push_str(&text.unescape().unwrap_or_default());
                    }
                    _ => {}
                }
                paragraph.events.push(event.into_owned());
            }
            None => {
                writer
                    .write_event(event)
                    .map_err(|error| AppError::Other(error.to_string()))?;
            }
        }
    }

    let mut out = String::from_utf8(writer.into_inner().into_inner())
        .map_err(|error| AppError::Other(error.to_string()))?;

    // Les ajouts se font en fin de corps, juste avant `w:sectPr` s'il existe.
    let mut ajouts = String::new();
    for operation in operations {
        match operation {
            Operation::Append { text, heading } => {
                ajouts.push_str(&paragraph_xml(text, *heading));
                report.paragraphs_added += 1;
            }
            Operation::ImagePlaceholder { description } => {
                ajouts.push_str(&placeholder_xml(description));
                report.placeholders_added += 1;
            }
            _ => {}
        }
    }
    if !ajouts.is_empty() {
        out = match out.rfind("<w:sectPr") {
            Some(position) => format!("{}{ajouts}{}", &out[..position], &out[position..]),
            None => match out.rfind("</w:body>") {
                Some(position) => format!("{}{ajouts}{}", &out[..position], &out[position..]),
                None => return Err(AppError::Invalid("Document Word inattendu.".into())),
            },
        };
    }
    Ok(out)
}

fn attribute(start: &BytesStart, name: &[u8]) -> Option<String> {
    start.attributes().flatten().find_map(|attribute| {
        (attribute.key.as_ref() == name)
            .then(|| String::from_utf8_lossy(&attribute.value).into_owned())
    })
}

/// Écrit un paragraphe, éventuellement retouché.
fn write_paragraph(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    paragraph: Paragraph,
    operations: &[Operation],
    report: &mut EditReport,
) -> Result<()> {
    let mut events = paragraph.events;
    let mut touched = false;

    for operation in operations {
        match operation {
            Operation::Format { target, formatting }
                if !formatting.is_empty() && target.matches(&paragraph.style, &paragraph.text) =>
            {
                events = apply_formatting(events, formatting);
                touched = true;
            }
            Operation::Replace { from, to } if !from.is_empty() => {
                let (next, count) = replace_text(events, from, to);
                events = next;
                report.replacements += count;
            }
            _ => {}
        }
    }
    if touched {
        report.paragraphs_touched += 1;
    }

    writer
        .write_event(Event::Start(BytesStart::new("w:p")))
        .map_err(|error| AppError::Other(error.to_string()))?;
    for event in events {
        writer
            .write_event(event)
            .map_err(|error| AppError::Other(error.to_string()))?;
    }
    writer
        .write_event(Event::End(quick_xml::events::BytesEnd::new("w:p")))
        .map_err(|error| AppError::Other(error.to_string()))?;
    Ok(())
}

/// Injecte la mise en forme dans chaque `w:r` du paragraphe.
///
/// Word attend les propriétés de run dans `w:rPr`, en PREMIER dans le run. On
/// retire donc les propriétés concurrentes de même nom — et seulement
/// celles-là : gras, italique, couleur et taille ne s'écrasent pas l'un
/// l'autre.
fn apply_formatting(events: Vec<Event<'static>>, formatting: &Formatting) -> Vec<Event<'static>> {
    let mut out: Vec<Event<'static>> = Vec::with_capacity(events.len() + 8);
    let mut index = 0usize;
    while index < events.len() {
        let event = events[index].clone();
        let ouvre_run = matches!(&event, Event::Start(start) if start.name().as_ref() == b"w:r");
        out.push(event);
        index += 1;
        if !ouvre_run {
            continue;
        }
        // Un `w:rPr` déjà présent suit immédiatement : on le remplace par sa
        // version enrichie, sinon on en insère un.
        let mut existant: Vec<Event<'static>> = Vec::new();
        if let Some(Event::Start(start)) = events.get(index) {
            if start.name().as_ref() == b"w:rPr" {
                let mut profondeur = 0usize;
                while index < events.len() {
                    let courant = events[index].clone();
                    match &courant {
                        Event::Start(tag) if tag.name().as_ref() == b"w:rPr" => profondeur += 1,
                        Event::End(tag) if tag.name().as_ref() == b"w:rPr" => {
                            profondeur -= 1;
                            index += 1;
                            if profondeur == 0 {
                                break;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    existant.push(courant);
                    index += 1;
                }
            }
        }
        for event in run_properties(&existant, formatting) {
            out.push(event);
        }
    }
    out
}

/// Reconstruit `w:rPr` : les propriétés existantes non concernées sont gardées,
/// celles que l'utilisateur redéfinit sont remplacées.
fn run_properties(existant: &[Event<'static>], formatting: &Formatting) -> Vec<Event<'static>> {
    let remplace = |name: &[u8]| match name {
        b"w:color" => formatting.color.is_some(),
        b"w:b" => formatting.bold.is_some(),
        b"w:i" => formatting.italic.is_some(),
        b"w:sz" | b"w:szCs" => formatting.size_pt.is_some(),
        _ => false,
    };
    let mut out: Vec<Event<'static>> = vec![Event::Start(BytesStart::new("w:rPr"))];
    for event in existant {
        let ignorer = match event {
            Event::Start(start) | Event::Empty(start) => remplace(start.name().as_ref()),
            Event::End(end) => remplace(end.name().as_ref()),
            _ => false,
        };
        // On saute l'ouverture `w:rPr` déjà émise ci-dessus.
        let est_ouverture =
            matches!(event, Event::Start(start) if start.name().as_ref() == b"w:rPr");
        if !ignorer && !est_ouverture {
            out.push(event.clone());
        }
    }
    if let Some(color) = &formatting.color {
        let mut tag = BytesStart::new("w:color");
        tag.push_attribute(("w:val", color.as_str()));
        out.push(Event::Empty(tag.into_owned()));
    }
    if let Some(bold) = formatting.bold {
        let mut tag = BytesStart::new("w:b");
        tag.push_attribute(("w:val", if bold { "1" } else { "0" }));
        out.push(Event::Empty(tag.into_owned()));
    }
    if let Some(italic) = formatting.italic {
        let mut tag = BytesStart::new("w:i");
        tag.push_attribute(("w:val", if italic { "1" } else { "0" }));
        out.push(Event::Empty(tag.into_owned()));
    }
    if let Some(size) = formatting.size_pt {
        let demi = (size * 2).to_string();
        for name in ["w:sz", "w:szCs"] {
            let mut tag = BytesStart::new(name);
            tag.push_attribute(("w:val", demi.as_str()));
            out.push(Event::Empty(tag.into_owned()));
        }
    }
    out.push(Event::End(quick_xml::events::BytesEnd::new("w:rPr")));
    out
}

/// Remplace un texte dans les nœuds textuels du paragraphe.
///
/// Limite assumée : Word découpe parfois un mot sur plusieurs `w:t` (correction
/// orthographique, révisions). Un remplacement à cheval sur deux nœuds n'est
/// donc pas trouvé — on préfère ne rien faire plutôt que de recoller le
/// paragraphe et perdre sa mise en forme interne.
fn replace_text(events: Vec<Event<'static>>, from: &str, to: &str) -> (Vec<Event<'static>>, usize) {
    let mut count = 0usize;
    let out = events
        .into_iter()
        .map(|event| match &event {
            Event::Text(text) => {
                let actuel = text.unescape().unwrap_or_default().into_owned();
                if actuel.contains(from) {
                    count += actuel.matches(from).count();
                    Event::Text(
                        quick_xml::events::BytesText::new(&actuel.replace(from, to)).into_owned(),
                    )
                } else {
                    event
                }
            }
            _ => event,
        })
        .collect();
    (out, count)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn paragraph_xml(text: &str, heading: bool) -> String {
    let style = if heading {
        "<w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>"
    } else {
        ""
    };
    format!(
        "<w:p>{style}<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        escape(text)
    )
}

/// L'encadré qui tient la place d'une image.
///
/// Syn ne produit pas d'images — le dire est plus honnête que de livrer un
/// document où il manque quelque chose sans prévenir. L'encadré nomme ce qui
/// doit venir là, et l'utilisateur dépose son image à cet endroit précis.
fn placeholder_xml(description: &str) -> String {
    let bordure = "<w:pBdr>\
        <w:top w:val=\"dashed\" w:sz=\"8\" w:space=\"4\" w:color=\"808080\"/>\
        <w:left w:val=\"dashed\" w:sz=\"8\" w:space=\"4\" w:color=\"808080\"/>\
        <w:bottom w:val=\"dashed\" w:sz=\"8\" w:space=\"4\" w:color=\"808080\"/>\
        <w:right w:val=\"dashed\" w:sz=\"8\" w:space=\"4\" w:color=\"808080\"/></w:pBdr>";
    format!(
        "<w:p><w:pPr>{bordure}<w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:rPr><w:i w:val=\"1\"/><w:color w:val=\"808080\"/></w:rPr>\
         <w:t xml:space=\"preserve\">[ Emplacement d'image — {} ] Syn ne crée pas d'images : déposez la vôtre ici.</w:t>\
         </w:r></w:p>",
        escape(description)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un docx à deux paragraphes : un titre rouge, un corps normal.
    fn document_test(dir: &Path) -> std::path::PathBuf {
        let contenu = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
            <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>\
            <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>\
              <w:r><w:rPr><w:color w:val=\"FF0000\"/><w:b w:val=\"1\"/></w:rPr><w:t>Le titre</w:t></w:r></w:p>\
            <w:p><w:r><w:t>Un paragraphe de corps.</w:t></w:r></w:p>\
            <w:sectPr/></w:body></w:document>";
        let chemin = dir.join("essai.docx");
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            for (nom, part) in [
                ("[Content_Types].xml", "<Types/>"),
                ("word/document.xml", contenu),
                // Une partie que Syn ne comprend pas : elle doit ressortir intacte.
                ("word/media/image1.png", "\u{89}PNG-FAUX"),
            ] {
                writer.start_file(nom, options).unwrap();
                writer.write_all(part.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        std::fs::write(&chemin, buffer.into_inner()).unwrap();
        chemin
    }

    fn document_xml(bytes: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
        let mut entry = archive.by_name("word/document.xml").unwrap();
        let mut texte = String::new();
        entry.read_to_string(&mut texte).unwrap();
        texte
    }

    /// Le cas de Paul : « les titres sont en rouge, mets-les en bleu, uniquement
    /// les titres ». Le corps ne doit pas bouger d'un octet.
    #[test]
    fn seuls_les_titres_changent_de_couleur() {
        let dir = std::env::temp_dir().join(format!("syn-docx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = document_test(&dir);

        let (bytes, rapport) = apply(
            &chemin,
            &[Operation::Format {
                target: Target::Headings,
                formatting: Formatting {
                    color: Some("0000FF".into()),
                    ..Default::default()
                },
            }],
        )
        .unwrap();
        let xml = document_xml(&bytes);

        assert_eq!(rapport.paragraphs_touched, 1, "un seul titre visé");
        assert!(xml.contains("w:color w:val=\"0000FF\""), "{xml}");
        assert!(
            !xml.contains("FF0000"),
            "l'ancienne couleur doit disparaître"
        );
        // Le gras du titre n'était pas concerné : il survit.
        assert!(xml.contains("w:b w:val=\"1\""), "{xml}");
        // Le corps n'est pas coloré.
        let corps = xml.split("Un paragraphe de corps").next().unwrap();
        assert_eq!(
            corps.matches("0000FF").count(),
            1,
            "seul le titre porte la nouvelle couleur"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ce que Syn ne comprend pas doit ressortir intact : images, styles,
    /// métadonnées. C'est la garantie qui rend l'édition acceptable.
    #[test]
    fn les_parties_non_touchees_sont_preservees() {
        let dir = std::env::temp_dir().join(format!("syn-docx2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = document_test(&dir);
        let (bytes, _) = apply(
            &chemin,
            &[Operation::Replace {
                from: "corps".into(),
                to: "texte".into(),
            }],
        )
        .unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut image = String::new();
        archive
            .by_name("word/media/image1.png")
            .unwrap()
            .read_to_string(&mut image)
            .unwrap();
        assert_eq!(image, "\u{89}PNG-FAUX");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ajouts_et_encadre_dimage_entrent_avant_la_fin_du_corps() {
        let dir = std::env::temp_dir().join(format!("syn-docx3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = document_test(&dir);
        let (bytes, rapport) = apply(
            &chemin,
            &[
                Operation::Append {
                    text: "Conclusion".into(),
                    heading: true,
                },
                Operation::ImagePlaceholder {
                    description: "graphique des ventes".into(),
                },
            ],
        )
        .unwrap();
        let xml = document_xml(&bytes);
        assert_eq!(rapport.paragraphs_added, 1);
        assert_eq!(rapport.placeholders_added, 1);
        assert!(xml.contains("Conclusion"), "{xml}");
        assert!(xml.contains("graphique des ventes"), "{xml}");
        assert!(
            xml.find("Conclusion").unwrap() < xml.find("<w:sectPr").unwrap(),
            "les ajouts restent dans le corps"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn un_titre_se_reconnait_quelle_que_soit_la_langue_de_word() {
        assert!(is_heading("Heading1"));
        assert!(is_heading("Titre2"));
        assert!(is_heading("berschrift3"));
        assert!(!is_heading("Normal"));
        assert!(!is_heading("ListParagraph"));
    }
}
