//! Extraction par type (Files §3). 🔎 figé pour ce build : pdf-extract (PDF),
//! zip+quick-xml (docx/pptx), calamine (xlsx), kamadak-exif (photos).
//! Tout échec = skip + log, jamais une chute du daemon (Files §7).

use std::io::Read;
use std::path::Path;

pub struct Extracted {
    pub text: Option<String>,
    pub kind: &'static str, // document | photo | media | code | other
    pub mime: String,
}

const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 400_000;

pub fn extract(path: &Path) -> Extracted {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" | "csv" | "log" | "json" | "yaml" | "yml" | "toml" | "tex"
        | "rtf" => Extracted {
            text: read_text(path),
            kind: "document",
            mime: mime_of(&ext),
        },
        "pdf" => Extracted {
            text: extract_pdf(path),
            kind: "document",
            mime: "application/pdf".into(),
        },
        "docx" => Extracted {
            text: extract_ooxml(path, "word/document.xml"),
            kind: "document",
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
        },
        "pptx" => Extracted {
            text: extract_pptx(path),
            kind: "document",
            mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .into(),
        },
        "xlsx" | "xls" | "ods" => Extracted {
            text: extract_sheet(path),
            kind: "document",
            mime: mime_of(&ext),
        },
        "jpg" | "jpeg" | "png" | "heic" | "tiff" | "gif" | "webp" => Extracted {
            text: extract_exif(path),
            kind: "photo",
            mime: format!("image/{ext}"),
        },
        "mp4" | "mov" | "avi" | "mkv" | "mp3" | "wav" | "m4a" | "flac" => {
            // Média lourd : métadonnées seules (transcription = connecteur Réunions [V2]).
            Extracted {
                text: None,
                kind: "media",
                mime: mime_of(&ext),
            }
        }
        "py" | "js" | "ts" | "tsx" | "jsx" | "rs" | "go" | "java" | "c" | "cpp" | "h" | "swift"
        | "rb" | "php" | "sh" => Extracted {
            text: read_text(path),
            kind: "code",
            mime: "text/plain".into(),
        },
        _ => Extracted {
            text: None,
            kind: "other",
            mime: "application/octet-stream".into(),
        },
    }
}

fn mime_of(ext: &str) -> String {
    match ext {
        "md" | "markdown" => "text/markdown".into(),
        "csv" => "text/csv".into(),
        "json" => "application/json".into(),
        _ => "text/plain".into(),
    }
}

fn cap(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(MAX_TEXT_CHARS).collect())
}

fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_TEXT_BYTES {
        // Gros fichier : on lit un préfixe, on n'embedde jamais 500 Mo d'un bloc.
        let mut f = std::fs::File::open(path).ok()?;
        let mut buf = vec![0u8; MAX_TEXT_BYTES as usize];
        let n = f.read(&mut buf).ok()?;
        buf.truncate(n);
        return cap(String::from_utf8_lossy(&buf).into_owned());
    }
    let bytes = std::fs::read(path).ok()?;
    cap(String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_pdf(path: &Path) -> Option<String> {
    // pdf-extract peut paniquer sur des PDF malformés → confinement.
    let p = path.to_path_buf();
    let result = std::panic::catch_unwind(move || pdf_extract::extract_text(&p));
    match result {
        Ok(Ok(text)) => cap(text),
        _ => None, // corrompu / protégé par mot de passe → métadonnées seules
    }
}

/// Extrait le texte des nœuds <w:t> / <a:t> d'un XML OOXML.
fn xml_text(xml: &str) -> String {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = e.name();
                let local = name.local_name();
                let local = String::from_utf8_lossy(local.as_ref()).to_string();
                if local == "t" {
                    in_text = true;
                } else if local == "p" || local == "br" {
                    out.push('\n');
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if String::from_utf8_lossy(e.name().local_name().as_ref()) == "t" {
                    in_text = false;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_text {
                    if let Ok(txt) = t.unescape() {
                        out.push_str(&txt);
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn extract_ooxml(path: &Path, inner: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name(inner).ok()?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml).ok()?;
    cap(xml_text(&xml))
}

fn extract_pptx(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .collect();
    let mut out = String::new();
    for name in names {
        if let Ok(mut entry) = zip.by_name(&name) {
            let mut xml = String::new();
            if entry.read_to_string(&mut xml).is_ok() {
                out.push_str(&xml_text(&xml));
                out.push('\n');
            }
        }
    }
    cap(out)
}

fn extract_sheet(path: &Path) -> Option<String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).ok()?;
    let mut out = String::new();
    let sheets = wb.sheet_names().to_vec();
    for sheet in sheets.iter().take(8) {
        if let Ok(range) = wb.worksheet_range(sheet) {
            out.push_str(&format!("# Feuille : {sheet}\n"));
            for row in range.rows().take(500) {
                let cells: Vec<String> = row.iter().map(|c| c.to_string()).collect();
                let line = cells.join(" | ");
                if !line
                    .trim_matches(|c: char| c == '|' || c.is_whitespace())
                    .is_empty()
                {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
    }
    cap(out)
}

/// Photos : EXIF (déterministe, fiable, gratuit — souvent le signal gagnant, Média §A5).
/// Embeddings de scène / visages : 🔎 modèle vision [V1 optionnel / V2] — dégradation gracieuse.
fn extract_exif(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let mut parts: Vec<String> = vec![];
    if let Some(f) = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY) {
        parts.push(format!("Prise le {}", f.display_value()));
    }
    let gps = |tag, r: &exif::Exif| -> Option<f64> {
        let field = r.get_field(tag, exif::In::PRIMARY)?;
        if let exif::Value::Rational(ref v) = field.value {
            if v.len() >= 3 {
                return Some(v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0);
            }
        }
        None
    };
    if let (Some(lat), Some(lon)) = (
        gps(exif::Tag::GPSLatitude, &exif),
        gps(exif::Tag::GPSLongitude, &exif),
    ) {
        let lat_ref = exif
            .get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        let lon_ref = exif
            .get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        let lat = if lat_ref.contains('S') { -lat } else { lat };
        let lon = if lon_ref.contains('W') { -lon } else { lon };
        parts.push(format!("GPS {:.5},{:.5}", lat, lon));
    }
    if let Some(f) = exif.get_field(exif::Tag::Model, exif::In::PRIMARY) {
        parts.push(format!("Appareil {}", f.display_value()));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}
