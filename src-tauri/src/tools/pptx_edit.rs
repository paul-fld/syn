//! Retouche d'une présentation PowerPoint EXISTANTE.
//!
//! Le texte d'une diapositive vit dans `<a:t>`, à l'intérieur de formes
//! (`<p:sp>`). Une forme sait si elle est un TITRE : son gabarit le déclare
//! (`<p:ph type="title"/>`). C'est ce qui permet de viser « les titres » sans
//! rien deviner — la même demande que sur un document Word, sur un objet qui
//! n'a rien à voir.
//!
//! Comme pour Word : seules les diapositives sont modifiées, et uniquement là
//! où une opération s'applique. Masques, dispositions, images, animations et
//! notes ressortent intacts.

use super::docx_edit::{EditReport, Formatting, Operation, Target};
use super::ooxml;
use crate::error::Result;
use std::path::Path;

pub fn apply(path: &Path, operations: &[Operation]) -> Result<(Vec<u8>, EditReport)> {
    let mut parts = ooxml::read_parts(path)?;
    let mut report = EditReport::default();
    let diapositives = ooxml::parts_matching(&parts, "ppt/slides/slide", ".xml");

    for nom in &diapositives {
        let Some(contenu) = ooxml::part_text(&parts, nom) else {
            continue;
        };
        let mut courant = contenu;
        for operation in operations {
            courant = match operation {
                Operation::Format { target, formatting } => {
                    let (suivant, touches) = format_shapes(&courant, target, formatting);
                    report.paragraphs_touched += touches;
                    suivant
                }
                Operation::Replace { from, to } if !from.is_empty() => {
                    let (suivant, count) = replace_in_text(&courant, from, to);
                    report.replacements += count;
                    suivant
                }
                _ => courant,
            };
        }
        ooxml::set_part(&mut parts, nom, courant);
    }

    // Ajouts et encadrés : sur la dernière diapositive, dans une forme neuve.
    let ajouts: Vec<String> = operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::Append { text, .. } => Some(text.clone()),
            Operation::ImagePlaceholder { description } => Some(format!(
                "[ Emplacement d'image — {description} ] Syn ne crée pas d'images : déposez la vôtre ici."
            )),
            _ => None,
        })
        .collect();
    if !ajouts.is_empty() {
        if let Some(nom) = diapositives.last() {
            if let Some(contenu) = ooxml::part_text(&parts, nom) {
                let mut courant = contenu;
                for (index, texte) in ajouts.iter().enumerate() {
                    courant = append_text_box(&courant, texte, index);
                }
                ooxml::set_part(&mut parts, nom, courant);
            }
        }
        for operation in operations {
            match operation {
                Operation::Append { .. } => report.paragraphs_added += 1,
                Operation::ImagePlaceholder { .. } => report.placeholders_added += 1,
                _ => {}
            }
        }
    }
    Ok((ooxml::write_parts(&parts)?, report))
}

/// Découpe la diapositive en formes et applique la mise en forme à celles qui
/// sont visées. Une forme non visée ressort caractère pour caractère.
fn format_shapes(xml: &str, target: &Target, formatting: &Formatting) -> (String, usize) {
    if formatting_is_empty(formatting) {
        return (xml.to_string(), 0);
    }
    let mut out = String::with_capacity(xml.len() + 256);
    let mut reste = xml;
    let mut touches = 0usize;
    while let Some(debut) = reste.find("<p:sp>") {
        let Some(fin_relative) = reste[debut..].find("</p:sp>") else {
            break;
        };
        let fin = debut + fin_relative + "</p:sp>".len();
        let forme = &reste[debut..fin];
        out.push_str(&reste[..debut]);
        let titre = forme.contains("type=\"title\"") || forme.contains("type=\"ctrTitle\"");
        let texte = shape_text(forme);
        let visé = match target {
            Target::Headings => titre,
            Target::Body => !titre,
            Target::All => true,
            Target::Containing(needle) => {
                crate::db::fold(&texte).contains(&crate::db::fold(needle))
            }
        };
        if visé && !texte.trim().is_empty() {
            out.push_str(&apply_run_properties(forme, formatting));
            touches += 1;
        } else {
            out.push_str(forme);
        }
        reste = &reste[fin..];
    }
    out.push_str(reste);
    (out, touches)
}

fn formatting_is_empty(formatting: &Formatting) -> bool {
    formatting.color.is_none()
        && formatting.bold.is_none()
        && formatting.italic.is_none()
        && formatting.size_pt.is_none()
}

/// Le texte visible d'une forme, concaténé.
fn shape_text(forme: &str) -> String {
    let mut texte = String::new();
    let mut reste = forme;
    while let Some(debut) = reste.find("<a:t>") {
        let apres = &reste[debut + 5..];
        let Some(fin) = apres.find("</a:t>") else {
            break;
        };
        texte.push_str(&apres[..fin]);
        reste = &apres[fin..];
    }
    texte
}

/// Injecte les propriétés de run (`<a:rPr>`) dans chaque run de la forme.
///
/// DrawingML veut la couleur dans un `<a:solidFill>` enfant, et le gras ou la
/// taille en attributs. Un `<a:rPr>` déjà présent est enrichi ; il n'est jamais
/// remplacé en entier, pour ne pas perdre la police ou la langue.
fn apply_run_properties(forme: &str, formatting: &Formatting) -> String {
    let mut attributs = String::new();
    if let Some(bold) = formatting.bold {
        attributs.push_str(&format!(" b=\"{}\"", if bold { 1 } else { 0 }));
    }
    if let Some(italic) = formatting.italic {
        attributs.push_str(&format!(" i=\"{}\"", if italic { 1 } else { 0 }));
    }
    if let Some(size) = formatting.size_pt {
        // DrawingML compte en centièmes de point.
        attributs.push_str(&format!(" sz=\"{}\"", size * 100));
    }
    let remplissage = formatting
        .color
        .as_ref()
        .map(|color| {
            format!(
                "<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>",
                color.trim_start_matches('#').to_uppercase()
            )
        })
        .unwrap_or_default();

    let mut out = String::with_capacity(forme.len() + 128);
    let mut reste = forme;
    while let Some(debut) = reste.find("<a:r>") {
        let contenu_debut = debut + "<a:r>".len();
        out.push_str(&reste[..contenu_debut]);
        reste = &reste[contenu_debut..];
        // Un `<a:rPr …/>` ou `<a:rPr …>…</a:rPr>` suit immédiatement, ou rien.
        if reste.starts_with("<a:rPr") {
            let auto_ferme = reste
                .find('>')
                .map(|position| reste[..position].ends_with('/'))
                .unwrap_or(false);
            if auto_ferme {
                let position = reste.find('>').unwrap();
                let ouverture = &reste[..position - 1];
                out.push_str(&format!("{ouverture}{attributs}>{remplissage}</a:rPr>"));
                reste = &reste[position + 1..];
            } else if let Some(position) = reste.find('>') {
                let ouverture = &reste[..position];
                out.push_str(&format!("{ouverture}{attributs}>{remplissage}"));
                reste = &reste[position + 1..];
            }
        } else {
            out.push_str(&format!("<a:rPr{attributs}>{remplissage}</a:rPr>"));
        }
    }
    out.push_str(reste);
    out
}

fn replace_in_text(xml: &str, from: &str, to: &str) -> (String, usize) {
    let mut out = String::with_capacity(xml.len());
    let mut reste = xml;
    let mut count = 0usize;
    while let Some(debut) = reste.find("<a:t>") {
        let contenu_debut = debut + "<a:t>".len();
        let Some(fin_relative) = reste[contenu_debut..].find("</a:t>") else {
            break;
        };
        let fin = contenu_debut + fin_relative;
        let texte = &reste[contenu_debut..fin];
        out.push_str(&reste[..contenu_debut]);
        if texte.contains(from) {
            count += texte.matches(from).count();
            out.push_str(&texte.replace(from, &ooxml::escape(to)));
        } else {
            out.push_str(texte);
        }
        reste = &reste[fin..];
    }
    out.push_str(reste);
    (out, count)
}

/// Ajoute une zone de texte à la diapositive, décalée pour ne pas recouvrir la
/// précédente. Les coordonnées sont en EMU (914 400 par pouce).
fn append_text_box(xml: &str, texte: &str, rang: usize) -> String {
    let y = 4_000_000 + rang as i64 * 700_000;
    let forme = format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{}\" name=\"Syn {}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
         <p:spPr><a:xfrm><a:off x=\"838200\" y=\"{y}\"/><a:ext cx=\"7000000\" cy=\"600000\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr>\
         <p:txBody><a:bodyPr wrap=\"square\"/><a:lstStyle/><a:p><a:r><a:rPr lang=\"fr-FR\"/><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>",
        900 + rang,
        rang + 1,
        ooxml::escape(texte)
    );
    match xml.rfind("</p:spTree>") {
        Some(position) => format!("{}{forme}{}", &xml[..position], &xml[position..]),
        None => xml.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn presentation(dir: &Path) -> std::path::PathBuf {
        let diapo = "<?xml version=\"1.0\"?><p:sld><p:cSld><p:spTree>\
            <p:sp><p:nvSpPr><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr>\
              <p:txBody><a:p><a:r><a:rPr lang=\"fr-FR\" b=\"1\"/><a:t>Ordre du jour</a:t></a:r></a:p></p:txBody></p:sp>\
            <p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr>\
              <p:txBody><a:p><a:r><a:t>Trois points à traiter</a:t></a:r></a:p></p:txBody></p:sp>\
            </p:spTree></p:cSld></p:sld>";
        let chemin = dir.join("reunion.pptx");
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            for (nom, part) in [
                ("[Content_Types].xml", "<Types/>"),
                ("ppt/slides/slide1.xml", diapo),
                ("ppt/slideMasters/slideMaster1.xml", "<p:sldMaster/>"),
                ("ppt/media/image1.png", "\u{89}PNG-FAUX"),
            ] {
                writer.start_file(nom, options).unwrap();
                std::io::Write::write_all(&mut writer, part.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        std::fs::write(&chemin, buffer.into_inner()).unwrap();
        chemin
    }

    fn partie(bytes: &[u8], nom: &str) -> String {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut texte = String::new();
        archive
            .by_name(nom)
            .unwrap()
            .read_to_string(&mut texte)
            .unwrap();
        texte
    }

    /// La même demande que sur un Word : seuls les titres changent, et le gras
    /// déjà présent sur le titre survit.
    #[test]
    fn seuls_les_titres_de_diapositive_changent_de_couleur() {
        let dir = std::env::temp_dir().join(format!("syn-pptx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = presentation(&dir);
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
        assert_eq!(rapport.paragraphs_touched, 1);
        let diapo = partie(&bytes, "ppt/slides/slide1.xml");
        assert!(diapo.contains("srgbClr val=\"0000FF\""), "{diapo}");
        assert!(
            diapo.contains("b=\"1\""),
            "le gras du titre survit : {diapo}"
        );
        // Le corps n'a pas reçu de couleur.
        let corps = diapo.split("Trois points").next().unwrap();
        assert_eq!(diapo.matches("srgbClr").count(), 1, "{corps}");
        // Une partie non comprise ressort intacte.
        assert_eq!(partie(&bytes, "ppt/media/image1.png"), "\u{89}PNG-FAUX");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn un_encadre_dimage_devient_une_zone_de_texte() {
        let dir = std::env::temp_dir().join(format!("syn-pptx2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = presentation(&dir);
        let (bytes, rapport) = apply(
            &chemin,
            &[Operation::ImagePlaceholder {
                description: "courbe de croissance".into(),
            }],
        )
        .unwrap();
        assert_eq!(rapport.placeholders_added, 1);
        let diapo = partie(&bytes, "ppt/slides/slide1.xml");
        assert!(diapo.contains("courbe de croissance"), "{diapo}");
        assert!(
            diapo.find("courbe de croissance").unwrap() < diapo.find("</p:spTree>").unwrap(),
            "la forme reste dans l'arbre de la diapositive"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn un_remplacement_ne_touche_que_le_texte_visible() {
        let dir = std::env::temp_dir().join(format!("syn-pptx3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = presentation(&dir);
        let (bytes, rapport) = apply(
            &chemin,
            &[Operation::Replace {
                from: "Trois".into(),
                to: "Quatre".into(),
            }],
        )
        .unwrap();
        assert_eq!(rapport.replacements, 1);
        let diapo = partie(&bytes, "ppt/slides/slide1.xml");
        assert!(diapo.contains("Quatre points"), "{diapo}");
        assert!(diapo.contains("type=\"title\""), "la structure est intacte");
        let _ = std::fs::remove_dir_all(dir);
    }
}
