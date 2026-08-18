//! Retouche d'un classeur Excel EXISTANT.
//!
//! Le piège d'un tableur : ses valeurs textuelles ne sont pas dans la feuille,
//! mais dans une table partagée (`xl/sharedStrings.xml`) que les cellules
//! référencent par index. Remplacer un texte se fait donc DANS cette table —
//! une seule fois, quel que soit le nombre de cellules qui l'utilisent — et les
//! formules, les styles et les graphiques n'y touchent pas.
//!
//! Ce que Syn ne fait volontairement pas : mettre en forme des cellules. Les
//! styles vivent dans `xl/styles.xml` sous forme d'un catalogue indexé ;
//! y injecter un format sans le comprendre entièrement produirait un classeur
//! que Excel refuse d'ouvrir. Mieux vaut le dire que le casser.

use super::docx_edit::{EditReport, Operation};
use super::ooxml;
use crate::error::{AppError, Result};
use std::path::Path;

pub fn apply(path: &Path, operations: &[Operation]) -> Result<(Vec<u8>, EditReport)> {
    let mut parts = ooxml::read_parts(path)?;
    let mut report = EditReport::default();

    for operation in operations {
        match operation {
            Operation::Replace { from, to } if !from.is_empty() => {
                report.replacements += replace_everywhere(&mut parts, from, to);
            }
            Operation::Append { text, .. } => {
                append_row(&mut parts, text)?;
                report.paragraphs_added += 1;
            }
            Operation::Format { .. } => {
                return Err(AppError::Invalid(
                    "Dans un classeur Excel, je sais remplacer une valeur et ajouter une ligne, mais pas encore mettre en forme des cellules sans risquer d'abîmer le fichier.".into(),
                ))
            }
            Operation::ImagePlaceholder { .. } => {
                return Err(AppError::Invalid(
                    "Un emplacement d'image n'a pas de sens dans un classeur : dis-moi plutôt dans quelle cellule écrire.".into(),
                ))
            }
            // Un remplacement vide ne fait rien, et ne doit pas échouer.
            Operation::Replace { .. } => {}
        }
    }
    Ok((ooxml::write_parts(&parts)?, report))
}

/// Remplace un texte dans la table partagée ET dans les cellules à texte
/// direct (`t="inlineStr"` ou `t="str"` pour les résultats de formule).
fn replace_everywhere(parts: &mut [(String, Vec<u8>)], from: &str, to: &str) -> usize {
    let mut count = 0usize;
    let mut cibles = vec!["xl/sharedStrings.xml".to_string()];
    cibles.extend(ooxml::parts_matching(parts, "xl/worksheets/sheet", ".xml"));
    for nom in cibles {
        let Some(contenu) = ooxml::part_text(parts, &nom) else {
            continue;
        };
        // Uniquement à l'intérieur des nœuds <t> : remplacer dans le XML brut
        // toucherait aussi des noms de feuille ou des attributs.
        let (nouveau, trouves) = replace_in_text_nodes(&contenu, from, to);
        if trouves > 0 {
            count += trouves;
            ooxml::set_part(parts, &nom, nouveau);
        }
    }
    count
}

/// Remplacement borné aux contenus textuels `<t …>…</t>`.
fn replace_in_text_nodes(xml: &str, from: &str, to: &str) -> (String, usize) {
    let mut out = String::with_capacity(xml.len());
    let mut reste = xml;
    let mut count = 0usize;
    while let Some(debut) = reste.find("<t") {
        // `<t>` ou `<t xml:space="preserve">`, mais pas `<tableParts>`.
        let apres = &reste[debut + 2..];
        let ferme = match apres.find('>') {
            Some(position) if apres[..position].chars().all(|c| c != '<') => position,
            _ => {
                out.push_str(&reste[..debut + 2]);
                reste = &reste[debut + 2..];
                continue;
            }
        };
        let nom_valide = apres[..ferme]
            .chars()
            .next()
            .map(|c| c == '>' || c.is_whitespace() || c == '/')
            .unwrap_or(true);
        let contenu_debut = debut + 2 + ferme + 1;
        if !nom_valide || apres[..ferme].ends_with('/') {
            out.push_str(&reste[..contenu_debut]);
            reste = &reste[contenu_debut..];
            continue;
        }
        let Some(fin_relative) = reste[contenu_debut..].find("</t>") else {
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
        out.push_str("</t>");
        reste = &reste[fin + 4..];
    }
    out.push_str(reste);
    (out, count)
}

/// Ajoute une ligne à la fin de la première feuille. Les valeurs sont séparées
/// par `;` ou une tabulation — ce que l'utilisateur écrit naturellement.
///
/// Les cellules sont écrites en texte littéral (`inlineStr`) : cela évite de
/// toucher à la table partagée, dont les index sont utilisés par tout le
/// classeur.
fn append_row(parts: &mut [(String, Vec<u8>)], text: &str) -> Result<()> {
    let feuille = ooxml::parts_matching(parts, "xl/worksheets/sheet", ".xml")
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Invalid("Ce classeur n'a aucune feuille lisible.".into()))?;
    let contenu = ooxml::part_text(parts, &feuille)
        .ok_or_else(|| AppError::Invalid("Feuille illisible.".into()))?;

    let prochaine = derniere_ligne(&contenu) + 1;
    let cellules: String = text
        .split([';', '\t'])
        .enumerate()
        .map(|(index, valeur)| {
            let colonne = colonne_excel(index);
            format!(
                "<c r=\"{colonne}{prochaine}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                ooxml::escape(valeur.trim())
            )
        })
        .collect();
    let ligne = format!("<row r=\"{prochaine}\">{cellules}</row>");

    let nouveau = match contenu.rfind("</sheetData>") {
        Some(position) => format!("{}{ligne}{}", &contenu[..position], &contenu[position..]),
        // Une feuille vide n'a parfois qu'une balise auto-fermante.
        None => contenu.replace("<sheetData/>", &format!("<sheetData>{ligne}</sheetData>")),
    };
    ooxml::set_part(parts, &feuille, nouveau);
    Ok(())
}

/// Le numéro de la dernière ligne utilisée, lu sur les attributs `r` des lignes.
fn derniere_ligne(xml: &str) -> u32 {
    let mut max = 0u32;
    let mut reste = xml;
    while let Some(position) = reste.find("<row ") {
        reste = &reste[position + 5..];
        if let Some(debut) = reste.find("r=\"") {
            let apres = &reste[debut + 3..];
            if let Some(fin) = apres.find('"') {
                if let Ok(numero) = apres[..fin].parse::<u32>() {
                    max = max.max(numero);
                }
            }
        }
    }
    max
}

/// 0 → A, 25 → Z, 26 → AA.
fn colonne_excel(index: usize) -> String {
    let mut reste = index;
    let mut nom = String::new();
    loop {
        nom.insert(0, (b'A' + (reste % 26) as u8) as char);
        if reste < 26 {
            break;
        }
        reste = reste / 26 - 1;
    }
    nom
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn classeur(dir: &Path) -> std::path::PathBuf {
        let feuille = "<?xml version=\"1.0\"?><worksheet><sheetData>\
            <row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c></row>\
            <row r=\"2\"><c r=\"A2\"><f>SUM(B1:B9)</f><v>42</v></c></row>\
            </sheetData></worksheet>";
        let partagees =
            "<?xml version=\"1.0\"?><sst count=\"1\"><si><t>Chiffre d'affaires</t></si></sst>";
        let chemin = dir.join("budget.xlsx");
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            for (nom, part) in [
                ("[Content_Types].xml", "<Types/>"),
                ("xl/worksheets/sheet1.xml", feuille),
                ("xl/sharedStrings.xml", partagees),
                ("xl/charts/chart1.xml", "<c:chart/>"),
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

    /// Le texte d'un tableur vit dans la table partagée : c'est là qu'il faut
    /// le remplacer, et nulle part ailleurs.
    #[test]
    fn un_remplacement_passe_par_la_table_partagee_sans_toucher_aux_formules() {
        let dir = std::env::temp_dir().join(format!("syn-xlsx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = classeur(&dir);
        let (bytes, rapport) = apply(
            &chemin,
            &[Operation::Replace {
                from: "Chiffre d'affaires".into(),
                to: "Revenus".into(),
            }],
        )
        .unwrap();
        assert_eq!(rapport.replacements, 1);
        assert!(partie(&bytes, "xl/sharedStrings.xml").contains("Revenus"));
        // La formule et le graphique ne bougent pas.
        assert!(partie(&bytes, "xl/worksheets/sheet1.xml").contains("SUM(B1:B9)"));
        assert_eq!(partie(&bytes, "xl/charts/chart1.xml"), "<c:chart/>");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn une_ligne_ajoutee_se_place_apres_la_derniere() {
        let dir = std::env::temp_dir().join(format!("syn-xlsx2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = classeur(&dir);
        let (bytes, rapport) = apply(
            &chemin,
            &[Operation::Append {
                text: "Mars ; 1200 ; validé".into(),
                heading: false,
            }],
        )
        .unwrap();
        assert_eq!(rapport.paragraphs_added, 1);
        let feuille = partie(&bytes, "xl/worksheets/sheet1.xml");
        assert!(feuille.contains("<row r=\"3\">"), "{feuille}");
        assert!(feuille.contains("r=\"C3\""), "trois colonnes : {feuille}");
        assert!(feuille.contains("Mars"), "{feuille}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn les_colonnes_suivent_lalphabet_dexcel() {
        assert_eq!(colonne_excel(0), "A");
        assert_eq!(colonne_excel(25), "Z");
        assert_eq!(colonne_excel(26), "AA");
    }

    /// Mettre en forme une cellule est refusé — explicitement, plutôt que de
    /// produire un classeur qu'Excel n'ouvre pas.
    #[test]
    fn la_mise_en_forme_de_cellules_est_refusee_franchement() {
        let dir = std::env::temp_dir().join(format!("syn-xlsx3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let chemin = classeur(&dir);
        let erreur = apply(
            &chemin,
            &[Operation::Format {
                target: super::super::docx_edit::Target::All,
                formatting: Default::default(),
            }],
        )
        .unwrap_err();
        assert!(erreur.to_string().contains("pas encore"), "{erreur}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
