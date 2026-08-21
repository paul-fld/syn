//! Retrieval hybride (Intelligence §6) : SQL structuré + sémantique (vecteurs),
//! fusion & ranking, budget de tokens strict, assemblage SOURCÉ.
//! Règle d'or : un bon retrieval bat un gros modèle.

use crate::db::Db;
use crate::error::Result;
use crate::llm::{blob_to_vec, cosine, LlmClient};
use crate::security::provenance;
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct Retrieved {
    pub item_id: String,
    pub source: String,
    pub source_ref: String,
    pub title: String,
    pub path: Option<String>,
    pub snippet: String,
    pub score: f32,
}

const CONTEXT_CHAR_BUDGET: usize = 9000; // budget strict (dépend du palier modèle)
const TOP_N: usize = 8;

/// Mots vides français : sans ce filtre, « je recherche un document avec mes
/// quittances » matche tous les documents contenant « avec » ou « mes » et le
/// bruit évince les vrais résultats du LIMIT.
const STOPWORDS_FR: &[&str] = &[
    "les",
    "des",
    "une",
    "aux",
    "ces",
    "mes",
    "tes",
    "ses",
    "nos",
    "vos",
    "leur",
    "leurs",
    "mon",
    "ton",
    "son",
    "que",
    "qui",
    "quoi",
    "dont",
    "mais",
    "donc",
    "car",
    "pour",
    "par",
    "sur",
    "sous",
    "dans",
    "avec",
    "sans",
    "vers",
    "chez",
    "est",
    "sont",
    "suis",
    "etait",
    "ete",
    "etre",
    "avoir",
    "fait",
    "faire",
    "peux",
    "peut",
    "veux",
    "veut",
    "dois",
    "doit",
    "sais",
    "sait",
    "plus",
    "moins",
    "tres",
    "bien",
    "tout",
    "tous",
    "toute",
    "toutes",
    "comme",
    "aussi",
    "alors",
    "ici",
    "cela",
    "cette",
    "celui",
    "celle",
    "ils",
    "elles",
    "nous",
    "vous",
    "moi",
    "toi",
    "lui",
    "elle",
    "ils",
    "quand",
    "comment",
    "pourquoi",
    "quel",
    "quelle",
    "quels",
    "quelles",
    "the",
    "and",
    "was",
    "recherche",
    "cherche",
    "trouve",
    "trouver",
    "retrouve",
    "retrouver",
    "ressors",
    "ressortir",
    "montre",
    "montrer",
    "ouvre",
    "ouvrir",
    "document",
    "documents",
    "fichier",
    "fichiers",
    "dossier",
    "normalement",
    "concernant",
    "lien",
    "lie",
    "lies",
    "pas",
    "parviens",
    "parvient",
    "parvenez",
    "rien",
    "correspond",
    "correspondre",
    "demande",
    "souhaite",
    "souhaitez",
    "information",
    "informations",
    "specifique",
    "traite",
    "traiter",
    "appelle",
    "appele",
    "nomme",
    "range",
    "ranger",
    "donne",
    "donner",
    // « cours » a été retiré de cette liste : c'est un NOM qui titre de vrais
    // documents (« Cours 2 », « Cours de droit »), pas seulement la locution
    // « en cours ». Le laisser ici rendait ces documents introuvables par leur
    // nom, et contredisait le routeur qui traite « cours » comme un mot de
    // recherche documentaire.
];

/// Radical naïf mais efficace en français : les pluriels réguliers tombent
/// (« quittances » → « quittance »), donc la requête au pluriel matche un
/// contenu au singulier et inversement (LIKE '%quittance%').
fn stem(word: &str) -> String {
    let w = word.to_string();
    if w.chars().count() >= 5 && (w.ends_with('s') || w.ends_with('x')) {
        let mut cs = w.chars();
        cs.next_back();
        cs.as_str().to_string()
    } else {
        w
    }
}

/// Vocabulaire de la DEMANDE : verbes de requête, pronoms, et noms de services.
/// Ces mots disent comment l'utilisateur formule sa question et où il croit que
/// la chose se trouve — jamais ce qu'il cherche. Français et anglais, parce que
/// la même demande peut arriver dans l'une ou l'autre langue.
///
/// Cette liste est délibérément la SEULE connaissance figée du chemin de
/// recherche. Tout le reste (le sujet, son titre, son emplacement réel) est
/// dérivé de la demande, jamais énuméré.
pub(crate) fn is_request_filler(word: &str) -> bool {
    matches!(
        word,
        // Formulation, français.
        "peux" | "peut" | "peu" | "veux" | "veut" | "voudrais" | "faut" | "pourrais"
        | "cherche" | "chercher" | "recherche" | "trouve" | "trouver" | "retrouve"
        | "retrouver" | "ressors" | "ressortir" | "sortir" | "montre" | "montrer"
        | "affiche" | "afficher" | "ouvre" | "ouvrir" | "donne" | "donner"
        | "rappelle" | "stp" | "svp" | "merci"
        | "faudrait" | "faudra"
        // Pronoms et déterminants : classes FERMÉES de la langue, donc
        // énumérables sans arbitraire — contrairement aux verbes, dont on
        // n'attrapera jamais toutes les formes.
        | "moi" | "toi" | "lui" | "tu" | "je" | "il" | "elle" | "on" | "me"
        | "te" | "se" | "nous" | "vous" | "ils" | "elles" | "ce" | "ceci"
        | "cela" | "ca" | "celui" | "celle" | "mes" | "mon" | "ma" | "tes"
        | "ton" | "ta" | "ces" | "ses" | "son" | "sa" | "cette" | "cet"
        | "que" | "qui" | "quoi" | "dont" | "quel" | "quelle" | "mien"
        // Les mots qui nomment le contenant, pas le contenu.
        //
        // « mail », « message », « courriel » en font partie : dans « retrouve
        // le mail de Liverpool », le mot « mail » dit OÙ chercher, jamais QUOI.
        // Envoyé à Gmail, il ramenait la boîte entière — presque tous les
        // messages contiennent « mail » dans leur pied de page.
        | "document" | "documents" | "fichier" | "fichiers" | "dossier" | "dossiers"
        | "mail" | "mails" | "email" | "emails" | "courriel" | "courriels"
        | "message" | "messages" | "inbox" | "boite"
        // Formulation, anglais.
        | "the" | "and" | "where" | "what" | "which" | "please" | "can" | "you"
        | "find" | "show" | "open" | "get" | "give" | "search" | "looking"
        | "file" | "files" | "doc" | "docs" | "spreadsheet" | "presentation"
        // Noms de services : ils désignent l'emplacement, pas le sujet.
        | "google" | "drive" | "gdrive" | "sheets" | "slides" | "gdocs"
        | "microsoft" | "onedrive" | "sharepoint" | "office" | "icloud"
        | "word" | "excel" | "powerpoint" | "outlook" | "mac"
    )
}

/// Mots grammaticaux qui LIENT deux mots porteurs. Contrairement aux mots de
/// formulation, ils appartiennent au titre cherché (« Jeu **de la** Vie ») :
/// on les conserve à l'intérieur d'une expression, jamais à ses extrémités.
pub(crate) fn is_connective(word: &str) -> bool {
    matches!(
        word,
        "de" | "du" | "des" | "la" | "le" | "les" | "un" | "une" | "au" | "aux"
            | "et" | "en" | "sur" | "pour" | "avec" | "dans" | "chez" | "vers"
            | "of" | "for" | "with" | "from" | "in" | "on" | "at" | "to" | "is"
    )
}

/// Extrait de la demande la plus longue expression continue qui ne contient
/// aucun mot de formulation — c'est-à-dire le sujet, tel que l'utilisateur l'a
/// écrit, ponctuation et casse comprises.
///
/// Cette règle ne connaît aucune tournure : elle segmente sur des CLASSES de
/// mots. « ressors-moi le document du Jeu de la Vie qui se trouve dans mes
/// Google Docs » et « where is the Q3 revenue forecast » passent par le même
/// chemin, sans qu'aucune de ces deux phrases n'ait été prévue.
///
/// Renvoie `None` si la demande ne contient aucun mot porteur.
pub(crate) fn subject_span(query: &str) -> Option<String> {
    #[derive(PartialEq)]
    enum Kind {
        Filler,
        Connective,
        Content,
    }
    let words: Vec<(&str, Kind)> = query
        .split_whitespace()
        .map(|word| {
            let bare = word.trim_matches(|character: char| !character.is_alphanumeric());
            let folded = crate::db::fold(bare);
            // Un composé se juge sur ses parties : « montre-moi » et « ouvre-le »
            // sont de la formulation, « vade_mecum » et « compte-rendu » non.
            // L'ordre compte : une pure liaison (« de ») reste une liaison, mais
            // dès qu'un mot de formulation entre dans le composé, l'ensemble en
            // devient un — sinon « ouvre-le » passerait pour un titre.
            let parts = folded
                .split(['-', '\'', '’'])
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            // Un mot sans lettre ni chiffre (ponctuation isolée) ne porte rien.
            let kind = if parts.is_empty() || parts.iter().copied().all(is_connective) {
                Kind::Connective
            } else if parts
                .iter()
                .copied()
                .all(|part| is_request_filler(part) || is_connective(part))
            {
                Kind::Filler
            } else {
                Kind::Content
            };
            (word, kind)
        })
        .collect();

    let mut best: Option<(usize, usize, usize)> = None; // (mots porteurs, début, fin)
    let mut start = 0usize;
    for boundary in 0..=words.len() {
        let is_end = boundary == words.len() || words[boundary].1 == Kind::Filler;
        if !is_end {
            continue;
        }
        // Les liaisons aux extrémités appartiennent à la formulation (« du »
        // dans « document du Jeu… »), pas au titre : on les rogne.
        let mut from = start;
        let mut to = boundary;
        while from < to && words[from].1 == Kind::Connective {
            from += 1;
        }
        while to > from && words[to - 1].1 == Kind::Connective {
            to -= 1;
        }
        let weight = words[from..to]
            .iter()
            .filter(|(_, kind)| *kind == Kind::Content)
            .count();
        if weight > 0 && best.is_none_or(|(current, _, _)| weight > current) {
            best = Some((weight, from, to));
        }
        start = boundary + 1;
    }
    let (_, from, to) = best?;
    let span = words[from..to]
        .iter()
        .map(|(word, _)| *word)
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = span.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '?' | '!' | '.' | ':' | ',' | ';')
    });
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn keywords(query: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for raw in crate::db::fold(query).split(|c: char| !c.is_alphanumeric()) {
        // Un token court qui porte un chiffre (« Q3 », « T2 », « 26 ») est au
        // contraire le plus discriminant d'une demande.
        let chiffre = raw.chars().any(|c| c.is_ascii_digit());
        let minimum = if chiffre { 2 } else { 3 };
        if raw.chars().count() < minimum || STOPWORDS_FR.contains(&raw) {
            continue;
        }
        // Les mots de formulation et ceux qui nomment le CONTENANT (« mail »,
        // « fichier », « google ») ne sont jamais du contenu. Sans ce filtre,
        // « le mail de Liverpool » cherchait « mail » — présent dans le pied de
        // page de presque tous les messages.
        if is_request_filler(raw) {
            continue;
        }
        let k = stem(raw);
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// Un terme de recherche et sa rareté dans le corpus de l'utilisateur.
#[derive(Debug, Clone)]
pub struct RankedTerm {
    pub text: String,
    /// Nombre d'éléments contenant ce terme, plafonné (voir `DF_CAP`).
    pub frequency: i64,
}

impl RankedTerm {
    /// Poids d'un terme dans un score de pertinence. Un mot présent partout ne
    /// prouve rien ; un mot rare désigne presque à lui seul ce qu'on cherche.
    pub fn weight(&self) -> f32 {
        if self.frequency <= 0 {
            1.0
        } else {
            1.0 / (1.0 + (self.frequency as f32).ln())
        }
    }
}

/// Au-delà de ce plafond, un terme est « banal » : inutile de compter plus loin,
/// et le comptage reste borné quelle que soit la taille du corpus.
const DF_CAP: i64 = 400;

/// Termes de la demande, du plus DISTINCTIF au plus banal.
///
/// La distinctivité n'est pas devinée : elle est MESURÉE sur le corpus de
/// l'utilisateur. Dans sa messagerie, « mail » ou « décembre » apparaissent
/// partout, « liverpool » dans une poignée de messages — c'est donc ce
/// dernier qui porte la demande.
///
/// Cet ordre est ce qui permet ensuite de retirer des mots un par un sans
/// perdre le sujet : on abandonne toujours le plus banal d'abord.
pub fn ranked_terms(db: &Db, query: &str, source: Option<&str>) -> Vec<RankedTerm> {
    ranked_terms_from(db, query, &[], source)
}

/// Même chose, en tenant compte de mots proposés par le modèle — typiquement
/// les équivalents anglais d'une demande écrite en français.
///
/// Ces mots ne bénéficient d'AUCUNE faveur : ils passent par la même mesure de
/// rareté que les autres. Un mot inventé sera simplement absent du corpus, donc
/// classé « inconnu », donc jamais mis en tête de la recherche. C'est ce qui
/// permet de faire confiance au modèle sans lui donner le dernier mot.
pub fn ranked_terms_from(
    db: &Db,
    query: &str,
    extra: &[String],
    source: Option<&str>,
) -> Vec<RankedTerm> {
    let mut mots = keywords(query);
    for mot in extra {
        let plie = crate::db::fold(mot);
        let plie = stem(&plie);
        if !plie.is_empty() && !mots.contains(&plie) && !is_request_filler(&plie) {
            mots.push(plie);
        }
    }
    let mut terms: Vec<RankedTerm> = mots
        .into_iter()
        .map(|text| {
            let frequency = document_frequency(db, &text, source);
            RankedTerm { text, frequency }
        })
        .collect();
    // Un terme ABSENT du corpus local ne prouve pas qu'il est rare : il peut
    // simplement ne pas encore être indexé (une boîte cloud n'est jamais copiée
    // en entier). Le traiter comme le plus distinctif reviendrait à bâtir toute
    // la recherche sur le mot dont on ne sait rien. Il prend donc un rang
    // neutre : après les mots rares mais attestés, avant les mots banals.
    let inconnu = DF_CAP / 4;
    terms.sort_by_key(|term| {
        if term.frequency == 0 {
            inconnu
        } else {
            term.frequency
        }
    });
    terms
}

/// Combien d'éléments contiennent ce terme (plafonné à `DF_CAP`).
fn document_frequency(db: &Db, term: &str, source: Option<&str>) -> i64 {
    let source_clause = match source {
        Some("files") => " AND i.source='files'",
        Some("mail") => " AND i.source='mail'",
        Some("cloud") => " AND i.source='cloud'",
        Some(_) => " AND 0",
        None => "",
    };
    let sql = format!(
        "SELECT COUNT(*) FROM (
           SELECT f.item_id FROM items_fts f
           JOIN items i ON i.id = f.item_id
           WHERE items_fts MATCH ?1 AND i.status='active'{source_clause}
           LIMIT {DF_CAP}
         )"
    );
    db.read(|c| {
        let mut stmt = c.prepare(&sql)?;
        Ok(stmt
            .query_row([format!("\"{}\"*", term.replace('"', ""))], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0))
    })
    .unwrap_or(0)
}

/// Les écritures possibles d'une date citée dans la demande.
///
/// « le match du 2 décembre » doit pouvoir reconnaître « Wed 2 Dec 2026 » dans
/// un message écrit en anglais. Ce n'est pas un dictionnaire de tournures — la
/// liste des mois est un fait du calendrier, fermé et fini, au même titre que
/// les accents que `syn_fold` normalise.
pub fn date_variants(query: &str) -> Vec<String> {
    const MOIS: [(&str, &str, &str); 12] = [
        ("janvier", "january", "jan"),
        ("fevrier", "february", "feb"),
        ("mars", "march", "mar"),
        ("avril", "april", "apr"),
        ("mai", "may", "may"),
        ("juin", "june", "jun"),
        ("juillet", "july", "jul"),
        ("aout", "august", "aug"),
        ("septembre", "september", "sep"),
        ("octobre", "october", "oct"),
        ("novembre", "november", "nov"),
        ("decembre", "december", "dec"),
    ];
    let folded = crate::db::fold(query);
    let mots: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|mot| !mot.is_empty())
        .collect();
    let mut variants = vec![];
    for (index, mot) in mots.iter().enumerate() {
        let Some((rang, (fr, en, abrege))) = MOIS
            .iter()
            .enumerate()
            .find(|(_, (fr, en, abrege))| *mot == *fr || *mot == *en || *mot == *abrege)
        else {
            continue;
        };
        // Le jour peut précéder (« 2 décembre ») ou suivre (« december 2 »).
        let jour = mots
            .get(index.wrapping_sub(1))
            .filter(|_| index > 0)
            .and_then(|mot| mot.parse::<u32>().ok())
            .or_else(|| {
                mots.get(index + 1)
                    .and_then(|mot| mot.parse::<u32>().ok())
                    .filter(|jour| *jour <= 31)
            });
        let Some(jour) = jour.filter(|jour| (1..=31).contains(jour)) else {
            variants.push(fr.to_string());
            variants.push(en.to_string());
            continue;
        };
        let numero = rang + 1;
        for forme in [
            format!("{jour} {fr}"),
            format!("{jour} {en}"),
            format!("{jour} {abrege}"),
            format!("{en} {jour}"),
            format!("{abrege} {jour}"),
            format!("{jour}/{numero}"),
            format!("{jour:02}/{numero:02}"),
            format!("{numero}/{jour}"),
        ] {
            if !variants.contains(&forme) {
                variants.push(forme);
            }
        }
    }
    variants
}

fn metadata_keyword_hits(result: &Retrieved, kws: &[String]) -> usize {
    let metadata = crate::db::fold(&format!(
        "{} {}",
        result.title,
        result.path.as_deref().unwrap_or(&result.source_ref)
    ));
    kws.iter()
        .filter(|keyword| metadata.contains(keyword.as_str()))
        .count()
}

fn file_extension(result: &Retrieved) -> String {
    std::path::Path::new(result.path.as_deref().unwrap_or(&result.source_ref))
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_visual_extension(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "heic" | "tiff" | "gif" | "webp" | "bmp"
    )
}

fn is_native_document_extension(extension: &str) -> bool {
    matches!(
        extension,
        "pdf"
            | "doc"
            | "docx"
            | "odt"
            | "rtf"
            | "txt"
            | "md"
            | "xls"
            | "xlsx"
            | "ods"
            | "ppt"
            | "pptx"
            | "key"
            | "pages"
            | "numbers"
    )
}

/// Réordonne les fichiers selon l'intention et la qualité de la preuve.
/// Le nom et le chemin sont généralement plus discriminants que du texte OCR
/// aperçu dans une capture. Le format n'est qu'un a priori : le contenu reste
/// pris en compte et une demande explicite d'image neutralise cette préférence.
fn rerank_files(query: &str, kws: &[String], results: &mut [Retrieved]) {
    let folded_query = crate::db::fold(query);
    let asks_for_visual = [
        "capture",
        "screenshot",
        "photo",
        "image",
        "png",
        "jpg",
        "jpeg",
    ]
    .iter()
    .any(|term| folded_query.contains(term));
    let asks_for_document = !asks_for_visual
        && [
            "document",
            "pdf",
            "word",
            "tableur",
            "presentation",
            "texte",
        ]
        .iter()
        .any(|term| folded_query.contains(term));

    for result in results {
        let metadata_coverage = metadata_keyword_hits(result, kws) as f32 / kws.len().max(1) as f32;
        result.score += 0.45 * metadata_coverage;

        let extension = file_extension(result);
        if asks_for_document {
            if is_native_document_extension(&extension) {
                result.score += 0.35;
            } else if is_visual_extension(&extension) {
                result.score -= 0.40;
            }
        } else if asks_for_visual && is_visual_extension(&extension) {
            result.score += 0.30;
        }
    }
}

/// Recherche hybride sur la mémoire sémantique + structurée.
pub async fn search(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
) -> Result<Vec<Retrieved>> {
    search_scoped(db, Some(llm), query, limit, None).await
}

/// Variante bornée à une source. Le filtrage est effectué dans SQL et dans la
/// recherche vectorielle, avant le LIMIT : filtrer après coup pouvait laisser
/// des documents de projet évincer entièrement les fichiers recherchés.
pub async fn search_source(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    limit: usize,
    source: &str,
) -> Result<Vec<Retrieved>> {
    search_scoped(db, Some(llm), query, limit, Some(source)).await
}

/// Résultats immédiatement disponibles via FTS5/BM25 et métadonnées. Cette
/// fonction ne fait aucun appel LLM, aucune extraction et aucun embedding.
pub async fn search_lexical_source(
    db: &Db,
    query: &str,
    limit: usize,
    source: &str,
) -> Result<Vec<Retrieved>> {
    search_scoped(db, None, query, limit, Some(source)).await
}

async fn search_scoped(
    db: &Db,
    llm: Option<&Arc<dyn LlmClient>>,
    query: &str,
    limit: usize,
    source: Option<&str>,
) -> Result<Vec<Retrieved>> {
    let kws = keywords(query);
    let embed_model = crate::settings::load(db)?.embed_model;
    let mut scores: HashMap<String, Retrieved> = HashMap::new();
    // Un résultat sémantique peut paraître proche au modèle tout en étant sans
    // rapport pour l'utilisateur. On conserve donc séparément la preuve
    // lexicale issue du fichier afin de pouvoir l'exiger pour les recherches
    // documentaires explicites.
    let mut lexical_hits: HashMap<String, i64> = HashMap::new();
    let mut semantic_hits: HashMap<String, f32> = HashMap::new();
    let mut item_kinds: HashMap<String, String> = HashMap::new();
    let now = crate::db::now();
    // Liste blanche : la valeur est interpolée dans le SQL, elle ne peut donc
    // jamais venir d'une saisie libre. Oublier une source connue ici revient à
    // rendre tout son cache introuvable — c'est ce qui est arrivé à « cloud ».
    let source_clause = match source {
        Some("files") => " AND i.source='files'",
        Some("mail") => " AND i.source='mail'",
        Some("cloud") => " AND i.source='cloud'",
        Some(_) => " AND 0",
        None => "",
    };

    // — Voie structurée (SQL) : mots-clés sur titre/corps/chemin —
    if !kws.is_empty() {
        let like_clauses: Vec<String> = (0..kws.len())
            .map(|i| {
                format!(
                    "(syn_fold(COALESCE(i.title,'')) LIKE '%'||?{0}||'%' OR syn_fold(COALESCE(i.body,'')) LIKE '%'||?{0}||'%' OR syn_fold(COALESCE(i.path,'')) LIKE '%'||?{0}||'%')",
                    i + 1
                )
            })
            .collect();
        let fts_param = kws.len() + 1;
        let sql = format!(
            "SELECT i.id, i.source, i.source_ref, i.title, i.path, substr(COALESCE(i.body, ''), 1, 400), i.mtime,
                    ({}) AS hits
             FROM items i WHERE i.status='active'{source_clause}
               AND i.id IN (
                 SELECT item_id FROM items_fts
                 WHERE items_fts MATCH ?{fts_param}
                 ORDER BY bm25(items_fts) LIMIT 500
               )
               AND ({})
             ORDER BY hits DESC LIMIT 80",
            like_clauses
                .iter()
                .map(|c| format!("CASE WHEN {c} THEN 1 ELSE 0 END"))
                .collect::<Vec<_>>()
                .join(" + "),
            like_clauses.join(" OR ")
        );
        db.read(|c| {
            let mut stmt = c.prepare(&sql)?;
            let fts_query = kws
                .iter()
                .map(|keyword| format!("{keyword}*"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut params_vec: Vec<&dyn rusqlite::ToSql> =
                kws.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
            params_vec.push(&fts_query);
            let rows = stmt.query_map(params_vec.as_slice(), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, source, source_ref, title, path, snippet, mtime, hits) = row?;
                let recency = recency_boost(now, mtime);
                let coverage = hits as f32 / kws.len().max(1) as f32;
                let folded_name = crate::db::fold(&format!(
                    "{} {}",
                    title.as_deref().unwrap_or_default(),
                    path.as_deref().unwrap_or_default()
                ));
                let name_hits = kws
                    .iter()
                    .filter(|keyword| folded_name.contains(keyword.as_str()))
                    .count() as f32;
                // La couverture de la demande prime largement sur la récence :
                // une vieille quittance est plus pertinente qu'un README récent.
                let score =
                    0.72 * coverage + 0.18 * (name_hits / kws.len().max(1) as f32) + 0.10 * recency;
                lexical_hits.insert(id.clone(), hits);
                scores.insert(
                    id.clone(),
                    Retrieved {
                        item_id: id,
                        source,
                        source_ref,
                        title: title.unwrap_or_default(),
                        path,
                        snippet,
                        score,
                    },
                );
            }
            Ok(())
        })?;
    }

    // — Voie structurée (entités) : agenda, tâches, engagements, personnes.
    // La doc Intelligence §6.3 l'exige ; sans elle, « mon rendez-vous de mardi »
    // ne peut venir que d'un document.
    if !kws.is_empty() && source.is_none() {
        structured_entities(db, &kws, &mut scores)?;
    }

    // — Voie sémantique (vecteurs) — dégradation gracieuse si embeddings indisponibles.
    if let Some(llm) = llm {
        if let Ok(qvecs) = llm.embed(&[query.to_string()]).await {
            if let Some(qvec) = qvecs.first() {
                // Le balayage ne lit QUE les vecteurs. Charger aussi le texte
                // de chaque fragment coûtait, sur un corpus de quelques dizaines
                // de milliers de fragments, des dizaines de méga-octets de
                // chaînes construites puis jetées — à chaque question posée.
                // Les textes des meilleurs fragments sont relus ensuite, en un
                // seul aller-retour.
                let mut best: HashMap<String, (f32, i64)> = HashMap::new();
                db.read(|c| {
                    let mut stmt = c.prepare(&format!(
                        "SELECT e.item_id, e.chunk_index, e.vector FROM embeddings e
                     JOIN items i ON i.id = e.item_id
                     WHERE e.vector IS NOT NULL AND e.model = ?1 AND i.status = 'active'{}",
                        source_clause
                    ))?;
                    let mut rows = stmt.query([&embed_model])?;
                    while let Some(row) = rows.next()? {
                        let item_id: String = row.get(0)?;
                        let chunk_index: i64 = row.get(1)?;
                        let blob: Vec<u8> = row.get(2)?;
                        let sim = cosine(qvec, &blob_to_vec(&blob));
                        let entry = best.entry(item_id).or_insert((f32::MIN, 0));
                        if sim > entry.0 {
                            *entry = (sim, chunk_index);
                        }
                    }
                    Ok(())
                })?;
                // On ne relit le texte que des fragments réellement retenus.
                let mut retained: Vec<(String, f32, i64)> = best
                    .iter()
                    .map(|(item_id, (sim, chunk))| (item_id.clone(), *sim, *chunk))
                    .collect();
                retained.sort_by(|left, right| {
                    right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal)
                });
                retained.truncate(limit.max(TOP_N) * 4);
                let mut best: HashMap<String, (f32, String)> = HashMap::new();
                db.read(|c| {
                    let mut stmt = c.prepare(
                        "SELECT text FROM embeddings WHERE item_id=?1 AND model=?2 AND chunk_index=?3",
                    )?;
                    for (item_id, sim, chunk) in retained {
                        let text: String = stmt
                            .query_row(rusqlite::params![item_id, embed_model, chunk], |row| row.get(0))
                            .unwrap_or_default();
                        best.insert(item_id, (sim, text));
                    }
                    Ok(())
                })?;
                if !best.is_empty() {
                    type ItemMeta = (
                        String,
                        String,
                        String,
                        String,
                        Option<String>,
                        Option<String>,
                        Option<i64>,
                    );
                    let metas: Vec<ItemMeta> =
                    db.read(|c| {
                        let mut stmt = c.prepare(
                            "SELECT id, source, source_ref, type, title, path, mtime FROM items WHERE id = ?1",
                        )?;
                        let mut out = vec![];
                        for id in best.keys() {
                            if let Ok(m) = stmt.query_row(params![id], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, String>(1)?,
                                    r.get::<_, String>(2)?,
                                    r.get::<_, String>(3)?,
                                    r.get::<_, Option<String>>(4)?,
                                    r.get::<_, Option<String>>(5)?,
                                    r.get::<_, Option<i64>>(6)?,
                                ))
                            }) {
                                out.push(m);
                            }
                        }
                        Ok(out)
                    })?;
                    for (id, source, source_ref, kind, title, path, mtime) in metas {
                        let (sim, text) = best.get(&id).cloned().unwrap_or((0.0, String::new()));
                        if sim < 0.35 {
                            continue; // bruit
                        }
                        semantic_hits.insert(id.clone(), sim);
                        item_kinds.insert(id.clone(), kind);
                        let add = 0.65 * sim + 0.2 * recency_boost(now, mtime);
                        scores
                            .entry(id.clone())
                            .and_modify(|r| {
                                r.score += add;
                                if !text.is_empty() {
                                    r.snippet = text.chars().take(600).collect();
                                }
                            })
                            .or_insert(Retrieved {
                                item_id: id,
                                source,
                                source_ref,
                                title: title.unwrap_or_default(),
                                path,
                                snippet: text.chars().take(600).collect(),
                                score: add,
                            });
                    }
                }
            }
        }
    }

    let mut out: Vec<Retrieved> = scores.into_values().collect();
    if source == Some("files") && !kws.is_empty() {
        // Pour une recherche de fichier, on préfère explicitement un faux
        // négatif expliqué à un faux positif absurde. Les embeddings servent à
        // classer les candidats prouvés par le contenu/nom/chemin, jamais à
        // injecter seuls un projet de code sans mot commun avec la demande.
        let min_hits = if kws.len() == 1 {
            1
        } else {
            kws.len().div_ceil(2).max(2)
        } as i64;
        out.retain(|result| {
            lexical_hits
                .get(&result.item_id)
                .is_some_and(|hits| *hits >= min_hits)
                // Un mot métier présent dans le nom ou le dossier constitue
                // une preuve forte, même si le document emploie un synonyme
                // pour le reste de la demande.
                || metadata_keyword_hits(result, &kws) > 0
                // Une forte proximité sémantique couvre les acronymes et les
                // paraphrases, mais uniquement pour les vrais documents : les
                // captures, médias et sources de code ne peuvent pas entrer
                // dans la sélection par cette voie seule.
                || (item_kinds
                    .get(&result.item_id)
                    .is_some_and(|kind| kind == "document")
                    && semantic_hits
                        .get(&result.item_id)
                        .is_some_and(|similarity| *similarity >= 0.72))
        });
        rerank_files(query, &kws, &mut out);
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit.max(1));
    Ok(out)
}

/// Recherche dans les entités structurées (hors items) : les résultats portent
/// un score fixe modeste — un hit exact sur un titre d'événement ou une tâche
/// est presque toujours pertinent.
fn structured_entities(
    db: &Db,
    kws: &[String],
    scores: &mut HashMap<String, Retrieved>,
) -> Result<()> {
    let clause = |cols: &[&str]| -> String {
        (0..kws.len())
            .map(|i| {
                cols.iter()
                    .map(|c| format!("syn_fold(COALESCE({c},'')) LIKE '%'||?{}||'%'", i + 1))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            })
            .map(|c| format!("({c})"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let params_vec: Vec<&dyn rusqlite::ToSql> =
        kws.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    db.read(|c| {
        // Événements d'agenda (miroir natif inclus).
        let sql = format!(
            "SELECT id, title, \"start\", COALESCE(location,''), COALESCE(source_ref, id) FROM events WHERE {} ORDER BY \"start\" DESC LIMIT 6",
            clause(&["title", "location", "notes"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, title, start, location, source_ref) = row?;
            let when = start
                .map(|t| {
                    chrono::DateTime::from_timestamp(t, 0)
                        .map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y %H:%M").to_string())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            scores.entry(format!("event:{id}")).or_insert(Retrieved {
                item_id: format!("event:{id}"),
                source: "calendar".into(),
                source_ref,
                title: title.clone().unwrap_or_default(),
                path: None,
                snippet: format!("Événement d'agenda : {} — {when} {location}", title.unwrap_or_default()),
                score: 0.45,
            });
        }
        // Tâches ouvertes.
        let sql = format!(
            "SELECT id, title, due, status FROM tasks WHERE {} ORDER BY due IS NULL, due LIMIT 6",
            clause(&["title"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, title, due, status) = row?;
            let due_s = due
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|d| format!(" (échéance {})", d.with_timezone(&chrono::Local).format("%d/%m/%Y")))
                .unwrap_or_default();
            scores.entry(format!("task:{id}")).or_insert(Retrieved {
                item_id: format!("task:{id}"),
                source: "tasks".into(),
                source_ref: format!("task:{id}"),
                title: title.clone(),
                path: None,
                snippet: format!("Tâche {status} : {title}{due_s}"),
                score: 0.45,
            });
        }
        // Engagements suivis.
        let sql = format!(
            "SELECT id, text, due, COALESCE(source_ref,'') FROM commitments WHERE {} ORDER BY rowid DESC LIMIT 4",
            clause(&["text"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, text, _due, source_ref) = row?;
            scores.entry(format!("commitment:{id}")).or_insert(Retrieved {
                item_id: format!("commitment:{id}"),
                source: "memory".into(),
                source_ref: if source_ref.is_empty() { format!("commitment:{id}") } else { source_ref },
                title: "Engagement".into(),
                path: None,
                snippet: format!("Engagement suivi : {text}"),
                score: 0.4,
            });
        }
        // Personnes connues (nom ou coordonnées).
        let sql = format!(
            "SELECT id, name, COALESCE(relationship,''), COALESCE(comm_channels,'') FROM people WHERE {} LIMIT 4",
            clause(&["name", "relationship", "comm_channels"])
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_vec.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, name, relationship, channels) = row?;
            scores.entry(format!("person:{id}")).or_insert(Retrieved {
                item_id: format!("person:{id}"),
                source: "people".into(),
                source_ref: format!("person:{id}"),
                title: name.clone(),
                path: None,
                snippet: format!(
                    "Personne connue : {name}{} {channels}",
                    if relationship.is_empty() { String::new() } else { format!(" ({relationship})") }
                ),
                score: 0.4,
            });
        }
        Ok(())
    })
}

fn recency_boost(now: i64, mtime: Option<i64>) -> f32 {
    match mtime {
        Some(t) => {
            let days = ((now - t).max(0) as f32) / 86_400.0;
            (1.0 / (1.0 + days / 30.0)).min(1.0)
        }
        None => 0.2,
    }
}

pub struct ContextBundle {
    /// (index de citation, fragment enveloppé « donnée non fiable »)
    pub fragments: Vec<(usize, String)>,
    pub sources: Vec<Retrieved>,
    /// Concaténation brute du contenu non fiable (pour l'analyse de dérivation).
    pub untrusted_text: String,
}

/// Assemblage borné + sourcé : chaque fragment garde son source_ref pour la citation.
pub async fn assemble(db: &Db, llm: &Arc<dyn LlmClient>, query: &str) -> Result<ContextBundle> {
    let results = search(db, llm, query, TOP_N).await?;
    assemble_results(results)
}

pub async fn assemble_source(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    query: &str,
    source: &str,
) -> Result<ContextBundle> {
    let results = search_source(db, llm, query, TOP_N, source).await?;
    assemble_results(results)
}

fn assemble_results(results: Vec<Retrieved>) -> Result<ContextBundle> {
    let mut fragments = vec![];
    let mut sources = vec![];
    let mut untrusted = String::new();
    let mut budget = CONTEXT_CHAR_BUDGET;
    for (i, r) in results.into_iter().enumerate() {
        let text = format!(
            "[source:{}] {} — {}\n{}",
            i + 1,
            r.title,
            r.source_ref,
            r.snippet
        );
        if text.len() > budget {
            break;
        }
        budget -= text.len();
        untrusted.push_str(&r.snippet);
        untrusted.push('\n');
        fragments.push((i + 1, provenance::wrap_untrusted(&r.source_ref, &text)));
        sources.push(r);
    }
    Ok(ContextBundle {
        fragments,
        sources,
        untrusted_text: untrusted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoLlm;
    #[async_trait::async_trait]
    impl LlmClient for NoLlm {
        async fn generate(
            &self,
            _s: &str,
            _m: &[crate::llm::ChatMessage],
            _t: &[crate::llm::ToolSpec],
            _p: crate::llm::GenParams,
        ) -> Result<crate::llm::LlmResponse> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn embed(&self, _t: &[String]) -> Result<Vec<Vec<f32>>> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn status(&self) -> crate::llm::LlmStatus {
            crate::llm::LlmStatus {
                available: false,
                runtime: "test".into(),
                chat_model_ready: false,
                embed_model_ready: false,
                installed_models: vec![],
                detail: None,
            }
        }
        async fn pull(&self, _m: &str, _p: tokio::sync::mpsc::Sender<(f32, String)>) -> Result<()> {
            Ok(())
        }
    }

    struct MisleadingSemanticLlm;
    #[async_trait::async_trait]
    impl LlmClient for MisleadingSemanticLlm {
        async fn generate(
            &self,
            _s: &str,
            _m: &[crate::llm::ChatMessage],
            _t: &[crate::llm::ToolSpec],
            _p: crate::llm::GenParams,
        ) -> Result<crate::llm::LlmResponse> {
            Err(crate::error::AppError::Other("hors ligne".into()))
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        async fn status(&self) -> crate::llm::LlmStatus {
            crate::llm::LlmStatus {
                available: true,
                runtime: "test".into(),
                chat_model_ready: false,
                embed_model_ready: true,
                installed_models: vec![],
                detail: None,
            }
        }
        async fn pull(&self, _m: &str, _p: tokio::sync::mpsc::Sender<(f32, String)>) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn les_mots_vides_et_pluriels_sont_normalises() {
        let kws = keywords("Je recherche un document en lien avec mes quittances de loyer");
        assert!(kws.contains(&"quittance".to_string()), "{kws:?}");
        assert!(kws.contains(&"loyer".to_string()), "{kws:?}");
        assert!(
            !kws.iter()
                .any(|k| k == "avec" || k == "mes" || k == "document"),
            "{kws:?}"
        );
    }

    #[test]
    fn la_normalisation_plie_accents_et_casse() {
        assert_eq!(
            crate::db::fold("Quittance de LOYER décembre"),
            "quittance de loyer decembre"
        );
        assert_eq!(stem("quittances"), "quittance");
        assert_eq!(stem("loyer"), "loyer");
    }

    #[tokio::test]
    async fn retrouve_une_quittance_au_pluriel_comme_au_singulier() {
        let dir = std::env::temp_dir().join(format!("syn-retrieval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('i1','files','/tmp/q.pdf','document','Mail_20251230_Quittance.pdf',
                         'Quittance de loyer — Redevance mensuelle du 01/12/2025',
                         '/Users/x/Documents/Travail/Quittances et factures/Mail_20251230_Quittance.pdf',
                         1, 'active')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(NoLlm);
        // Pluriel dans la requête, singulier dans le document : doit matcher.
        let res = search(&db, &llm, "mes quittances de loyer", 8)
            .await
            .unwrap();
        assert!(!res.is_empty(), "aucun résultat");
        assert_eq!(res[0].item_id, "i1");
        // Et avec du bruit conversationnel autour.
        let res = search(&db, &llm, "je cherche un document en lien avec mes quittances de loyer, tu peux me le retrouver ?", 8)
            .await
            .unwrap();
        assert!(
            !res.is_empty(),
            "le bruit conversationnel évince le résultat"
        );
        assert_eq!(res[0].item_id, "i1");
        // Le filtrage doit avoir lieu avant LIMIT. Quarante résultats mémoire
        // plus forts ne doivent pas faire disparaître l'unique fichier.
        db.with(|c| {
            for n in 0..45 {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1,'conversation',?2,'note','Projet Aberration',
                             'quittance loyer quittance loyer — documentation de projet',1,'active')",
                    params![format!("noise-{n}"), format!("project:{n}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let res = search_source(&db, &llm, "quittance loyer", 8, "files")
            .await
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].item_id, "i1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejette_un_projet_code_meme_si_lembedding_le_classe_premier() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-noise-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('code','files','/tmp/README.md','code','README.md',
                         'Documentation technique du projet Aberration et recette de déploiement.',
                         '/Users/x/Projets/Aberration/README.md',1,'active')",
                [],
            )?;
            c.execute(
                "INSERT INTO embeddings (item_id, model, chunk_index, text, vector)
                 VALUES ('code','test',0,'Projet Aberration',?1)",
                params![crate::llm::vec_to_blob(&[1.0, 0.0])],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(MisleadingSemanticLlm);
        let results = search_source(
            &db,
            &llm,
            "Retrouve un document lié à ma quittance de loyer",
            8,
            "files",
        )
        .await
        .unwrap();
        assert!(
            results.is_empty(),
            "un embedding seul ne doit jamais faire remonter {results:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn un_document_natif_prime_sur_des_captures_ocr_plus_recentes() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-ranking-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, mtime, ingested_at, status)
                 VALUES ('document','files','/tmp/Archives/Mail_2025_Quittance.pdf','document',
                         'Mail_2025_Quittance.pdf','Quittance de loyer — période mensuelle',
                         '/tmp/Archives/Quittances et factures/Mail_2025_Quittance.pdf',1,1,'active')",
                [],
            )?;
            for n in 0..8 {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, path, mtime, ingested_at, status)
                     VALUES (?1,'files',?2,'photo',?3,
                             'Capture d’une conversation : recherche de quittance de loyer',
                             ?2,9999999999,1,'active')",
                    params![
                        format!("capture-{n}"),
                        format!("/tmp/Capture écran {n}.png"),
                        format!("Capture écran {n}.png")
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(NoLlm);
        let results = search_source(
            &db,
            &llm,
            "Retrouve un document lié à ma quittance de loyer",
            8,
            "files",
        )
        .await
        .unwrap();
        assert_eq!(results[0].item_id, "document", "{results:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn retrouve_un_document_conceptuel_sans_nom_ni_emplacement_connus() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-concept-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES ('embed_model', '\"test\"')
                 ON CONFLICT(key) DO UPDATE SET value='\"test\"'",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('policy','files','/tmp/Interne/Reference_2025.pdf','document',
                         'Reference_2025.pdf',
                         'Politique de sécurité des systèmes d’information applicable au personnel',
                         '/tmp/Interne/Reference_2025.pdf',1,'active')",
                [],
            )?;
            c.execute(
                "INSERT INTO embeddings (item_id, model, chunk_index, text, vector)
                 VALUES ('policy','test',0,
                         'Politique de sécurité des systèmes d’information applicable au personnel',?1)",
                params![crate::llm::vec_to_blob(&[1.0, 0.0])],
            )?;
            c.execute(
                "INSERT INTO enrichment_queue(item_id,source,source_ref,state,base_priority,
                 lexical_ready,embedding_ready,updated_at)
                 VALUES ('policy','files','/tmp/Interne/Reference_2025.pdf','embedded',1,1,1,1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(MisleadingSemanticLlm);
        let results = search_source(
            &db,
            &llm,
            "Peux-tu retrouver le document sur la PSSI de mon entreprise ?",
            8,
            "files",
        )
        .await
        .unwrap();
        assert_eq!(results[0].item_id, "policy", "{results:#?}");
        let coverage: (i64, i64) = db
            .with(|c| {
                Ok((
                    c.query_row("SELECT COUNT(*) FROM enrichment_queue", [], |r| r.get(0))?,
                    c.query_row(
                        "SELECT SUM(embedding_ready) FROM enrichment_queue",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(
            coverage,
            (1, 1),
            "le document ancien devient sémantique lorsque la file est passée dessus"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recherche_lexicale_par_nom_sous_deux_secondes_sans_appeler_le_llm() {
        let dir =
            std::env::temp_dir().join(format!("syn-retrieval-volume-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"1".repeat(64)).unwrap();
        db.with(|connection| {
            let transaction = connection.unchecked_transaction()?;
            for index in 0..10_000 {
                transaction.execute(
                    "INSERT INTO items(id,source,source_ref,type,title,body,path,ingested_at,status)
                     VALUES (?1,'files',?2,'document',?3,NULL,?2,1,'active')",
                    params![format!("v{index}"), format!("/Documents/archive-{index}.pdf"),
                            if index == 9_999 { "Contrat Galaxie 2017" } else { "Archive ordinaire" }],
                )?;
            }
            transaction.commit()?;
            Ok(())
        }).unwrap();
        let started = std::time::Instant::now();
        let results = search_lexical_source(&db, "Contrat Galaxie", 8, "files")
            .await
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(
            results.first().map(|item| item.item_id.as_str()),
            Some("v9999")
        );
        let layers: (i64, i64) = db
            .with(|connection| {
                Ok((
                    connection.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE name='items_fts'",
                        [],
                        |row| row.get(0),
                    )?,
                    connection.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE name='embeddings'",
                        [],
                        |row| row.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(
            layers,
            (1, 1),
            "FTS/BM25 et embeddings doivent rester deux couches distinctes"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Régression : `search_lexical_source(..., "cloud")` compilait une clause
    /// `AND 0`. Tout le cache Google Drive / OneDrive était donc invisible, et
    /// une demande « le document du Jeu de la Vie » ne pouvait remonter que ce
    /// que le fournisseur voulait bien renvoyer en direct.
    #[tokio::test]
    async fn le_cache_cloud_est_reellement_cherchable() {
        let dir = std::env::temp_dir().join(format!("syn-cloud-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"3".repeat(64)).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('c1','cloud','google:drive:abc','document','Le Jeu de la Vie 2.0 - Sources',
                         'Nom : Le Jeu de la Vie 2.0\nType : application/vnd.google-apps.document',
                         'https://docs.google.com/document/d/abc/edit', 1, 'active')",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, path, ingested_at, status)
                 VALUES ('f1','files','/tmp/vie.txt','document','Voyage en martinique',
                         'notes de voyage', '/tmp/vie.txt', 1, 'active')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let found = search_lexical_source(&db, "Jeu de la Vie", 8, "cloud")
            .await
            .unwrap();
        assert_eq!(
            found.iter().map(|r| r.item_id.as_str()).collect::<Vec<_>>(),
            vec!["c1"],
            "le cache cloud doit répondre, et sans laisser passer un fichier local"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    /// La distinctivité se MESURE : dans une messagerie française, « mail » et
    /// « décembre » sont partout, « liverpool » dans une poignée de messages.
    /// C'est cet ordre qui permet ensuite de retirer des mots sans perdre le
    /// sujet de la demande.
    #[test]
    fn les_mots_porteurs_se_classent_par_rarete_reelle() {
        let dir = std::env::temp_dir().join(format!("syn-idf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"9".repeat(64)).unwrap();
        db.with(|c| {
            // Une boîte ordinaire : tout le monde parle de décembre et signe
            // ses messages d'un pied de page contenant « mail ».
            for index in 0..30 {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1,'mail',?1,'email','Nouvelle offre',
                             'Rendez-vous en decembre. Cet e-mail vous est adresse automatiquement.',
                             1,'active')",
                    rusqlite::params![format!("bruit{index}")],
                )?;
            }
            c.execute(
                "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                 VALUES ('lfc','mail','google:mail:1','email','Liverpool FC Booking Confirmation',
                         'Thank you for your booking with Liverpool Football Club. Ticket. Liverpool v Sunderland - Wed 2 Dec 2026',
                         1,'active')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let termes = ranked_terms(
            &db,
            "le mail de liverpool concernant mes tickets pour le match du 2 décembre",
            Some("mail"),
        );
        let mots: Vec<&str> = termes.iter().map(|t| t.text.as_str()).collect();
        assert!(
            !mots.contains(&"mail"),
            "« mail » nomme le contenant, pas le contenu : {mots:?}"
        );
        assert_eq!(
            mots.first().copied(),
            Some("liverpool"),
            "le mot le plus rare porte la demande : {mots:?}"
        );
        assert!(
            mots.iter().position(|m| *m == "ticket")
                < mots.iter().position(|m| *m == "decembre"),
            "« décembre » est banal dans cette boîte : {mots:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// « le match du 2 décembre » doit reconnaître « Wed 2 Dec 2026 » : le
    /// message est en anglais, la demande en français.
    #[test]
    fn une_date_citee_se_reconnait_dans_les_deux_langues() {
        let variantes = date_variants("mes tickets pour le match du 2 décembre");
        assert!(variantes.contains(&"2 dec".to_string()), "{variantes:?}");
        assert!(variantes.contains(&"2 december".to_string()), "{variantes:?}");
        assert!(variantes.contains(&"2 decembre".to_string()), "{variantes:?}");
        assert!(
            crate::db::fold("Liverpool v Sunderland - Wed 2 Dec 2026")
                .contains(variantes.iter().find(|v| *v == "2 dec").unwrap()),
        );
        assert!(
            date_variants("retrouve la facture d'électricité").is_empty(),
            "sans date citée, aucune variante"
        );
    }
}
