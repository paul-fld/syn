//! Ce que les formats OOXML partagent : ouvrir l'archive, remplacer une ou
//! plusieurs parties, tout recopier à l'identique.
//!
//! Word, Excel et PowerPoint sont trois grammaires différentes dans le même
//! emballage. Ce module tient l'emballage — la garantie « ne jamais toucher à
//! ce qu'on n'a pas compris » vit ici, une seule fois, pour les trois.

use crate::error::{AppError, Result};
use std::io::{Cursor, Read, Write};
use std::path::Path;

/// Toutes les parties d'une archive OOXML, dans l'ordre où elles s'y trouvent.
pub fn read_parts(path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let file = std::fs::File::open(path)
        .map_err(|error| AppError::NotFound(format!("Fichier illisible : {error}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| AppError::Invalid("Ce fichier n'est pas un document Office valide.".into()))?;
    let mut parts = Vec::with_capacity(archive.len());
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
    Ok(parts)
}

/// Réécrit l'archive complète. Une partie non modifiée ressort octet pour
/// octet : c'est ce qui rend la retouche acceptable sur un document de travail.
pub fn write_parts(parts: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
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
    Ok(buffer.into_inner())
}

/// Le texte XML d'une partie, s'il existe.
pub fn part_text(parts: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    parts
        .iter()
        .find(|(part, _)| part == name)
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
}

/// Remplace le contenu d'une partie.
pub fn set_part(parts: &mut [(String, Vec<u8>)], name: &str, content: String) {
    if let Some(entry) = parts.iter_mut().find(|(part, _)| part == name) {
        entry.1 = content.into_bytes();
    }
}

/// Les parties dont le nom correspond à un préfixe et une extension : les
/// feuilles d'un classeur, les diapositives d'une présentation.
pub fn parts_matching(parts: &[(String, Vec<u8>)], prefix: &str, suffix: &str) -> Vec<String> {
    let mut noms: Vec<String> = parts
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| name.starts_with(prefix) && name.ends_with(suffix))
        .collect();
    noms.sort();
    noms
}

/// Échappement XML minimal pour du texte inséré.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
