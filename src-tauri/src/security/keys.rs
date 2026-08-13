//! Clé maîtresse & récupération (doc maître §17).
//! - La clé K (32 octets aléatoires) chiffre la base (SQLCipher).
//! - K est enveloppée deux fois : par le mot de passe maître et par la phrase
//!   de récupération. Pas de récupération côté serveur (ce serait une porte dérobée).
//! - Optionnel : K dans le trousseau OS (déverrouillage par session/biométrie).
//! - Sans mot de passe ni phrase : données irrécupérables — annoncé clairement.

use crate::error::{AppError, Result};
use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "app.syn.desktop";
const KEYCHAIN_USER: &str = "master-key";

/// 256 mots français courants → phrase de récupération de 12 mots (96 bits).
const WORDLIST: &[&str] = &[
    "arbre",
    "avion",
    "ancre",
    "amande",
    "aurore",
    "abri",
    "acier",
    "aigle",
    "alpage",
    "ambre",
    "averse",
    "azur",
    "balise",
    "banc",
    "barque",
    "bassin",
    "berge",
    "bijou",
    "bison",
    "blague",
    "bleuet",
    "bosquet",
    "boussole",
    "brise",
    "brume",
    "bulle",
    "cabane",
    "cactus",
    "calanque",
    "canard",
    "capsule",
    "carnet",
    "cascade",
    "castor",
    "cerise",
    "chalet",
    "chemin",
    "cintre",
    "citron",
    "clairon",
    "climat",
    "cobalt",
    "colline",
    "comete",
    "congre",
    "corail",
    "cordage",
    "cormoran",
    "cosmos",
    "coteau",
    "coupole",
    "crabe",
    "craie",
    "cristal",
    "cumulus",
    "cygne",
    "dauphin",
    "delta",
    "dorade",
    "dune",
    "eclair",
    "ecorce",
    "ecume",
    "eden",
    "elan",
    "email",
    "envol",
    "epice",
    "erable",
    "escale",
    "etoile",
    "etui",
    "falaise",
    "fanal",
    "faucon",
    "fenetre",
    "fermoir",
    "feuille",
    "figue",
    "fjord",
    "flamme",
    "flocon",
    "fontaine",
    "foret",
    "fougere",
    "fourmi",
    "fresque",
    "frimas",
    "fusain",
    "galet",
    "gazelle",
    "geyser",
    "girafe",
    "givre",
    "glacier",
    "gorge",
    "goutte",
    "granit",
    "grange",
    "grive",
    "grotte",
    "guepard",
    "harpe",
    "havre",
    "heron",
    "hetre",
    "hibou",
    "horizon",
    "houle",
    "iceberg",
    "ilot",
    "iris",
    "ivoire",
    "jade",
    "jardin",
    "jasmin",
    "jetee",
    "jonquille",
    "jungle",
    "kayak",
    "lagon",
    "lande",
    "lanterne",
    "lavande",
    "lezard",
    "liane",
    "lichen",
    "liege",
    "lilas",
    "littoral",
    "loutre",
    "lueur",
    "lune",
    "lynx",
    "magma",
    "marais",
    "marbre",
    "maree",
    "melodie",
    "menthe",
    "meridien",
    "mesange",
    "meteore",
    "mica",
    "miel",
    "mimosa",
    "mirage",
    "mistral",
    "moulin",
    "mousse",
    "muguet",
    "murier",
    "nacre",
    "nectar",
    "neige",
    "nid",
    "nuage",
    "oasis",
    "ocean",
    "ocre",
    "olivier",
    "ombre",
    "onyx",
    "opale",
    "orage",
    "orchidee",
    "orme",
    "ortie",
    "osier",
    "otarie",
    "ouragan",
    "palmier",
    "panda",
    "papyrus",
    "pastel",
    "pature",
    "pelican",
    "pepite",
    "perle",
    "phare",
    "pigment",
    "pinede",
    "pivoine",
    "plaine",
    "planete",
    "plateau",
    "plume",
    "polaire",
    "pollen",
    "pomme",
    "poney",
    "prairie",
    "presqu",
    "prisme",
    "puits",
    "pumas",
    "quartz",
    "quiétude",
    "rafale",
    "rameau",
    "ravin",
    "recif",
    "renard",
    "reseda",
    "riviere",
    "rocher",
    "romarin",
    "rosee",
    "roseau",
    "rubis",
    "ruche",
    "ruisseau",
    "sable",
    "safran",
    "saphir",
    "sapin",
    "saule",
    "savane",
    "seiche",
    "sentier",
    "sequoia",
    "sirocco",
    "soleil",
    "sommet",
    "source",
    "sterne",
    "syrinx",
    "talus",
    "tamaris",
    "tanière",
    "tempete",
    "terrier",
    "thym",
    "tilleul",
    "tornade",
    "torrent",
    "toundra",
    "tourbe",
    "trefle",
    "tresor",
    "tulipe",
    "vague",
    "vallon",
    "vanille",
    "verger",
    "vermeil",
    "verriere",
    "vigne",
    "violette",
    "vipere",
    "volcan",
    "voilier",
    "zenith",
    "zephyr",
    "zircon",
    "cedre",
];

#[derive(Serialize, Deserialize, Clone)]
struct WrappedKey {
    salt: String,
    nonce: String,
    wrapped: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Meta {
    version: u32,
    pub email: Option<String>,
    pub created_at: i64,
    pw: WrappedKey,
    rp: WrappedKey,
    pub keychain: bool,
}

pub struct KeyStore {
    dir: PathBuf,
}

impl KeyStore {
    pub fn new(dir: &Path) -> Self {
        KeyStore {
            dir: dir.to_path_buf(),
        }
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("syn-meta.json")
    }

    pub fn exists(&self) -> bool {
        self.meta_path().exists()
    }

    pub fn meta(&self) -> Result<Meta> {
        let raw = std::fs::read_to_string(self.meta_path())
            .map_err(|_| AppError::NotFound("Syn n'est pas encore configuré.".into()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    fn save_meta(&self, meta: &Meta) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.meta_path(), serde_json::to_string_pretty(meta)?)?;
        Ok(())
    }

    /// First-run : crée K, l'enveloppe (mot de passe + phrase), écrit le meta.
    /// Retourne (clé hex, phrase de récupération).
    pub fn setup(&self, email: Option<String>, password: &str) -> Result<(String, String)> {
        if self.exists() {
            return Err(AppError::Invalid(
                "Syn est déjà configuré sur cette machine.".into(),
            ));
        }
        if password.len() < 8 {
            return Err(AppError::Invalid(
                "Le mot de passe maître doit faire au moins 8 caractères.".into(),
            ));
        }
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let phrase = generate_phrase();
        let meta = Meta {
            version: 1,
            email,
            created_at: chrono::Utc::now().timestamp(),
            pw: wrap(&key, password.as_bytes())?,
            rp: wrap(&key, phrase.as_bytes())?,
            keychain: false,
        };
        self.save_meta(&meta)?;
        Ok((hex::encode(key), phrase))
    }

    pub fn unlock_password(&self, password: &str) -> Result<String> {
        let meta = self.meta()?;
        let key = unwrap(&meta.pw, password.as_bytes())
            .map_err(|_| AppError::Security("Mot de passe maître incorrect.".into()))?;
        Ok(hex::encode(key))
    }

    pub fn unlock_phrase(&self, phrase: &str) -> Result<String> {
        let meta = self.meta()?;
        let normalized = phrase
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let key = unwrap(&meta.rp, normalized.as_bytes())
            .map_err(|_| AppError::Security("Phrase de récupération incorrecte.".into()))?;
        Ok(hex::encode(key))
    }

    /// Ré-enveloppe K avec un nouveau mot de passe (K obtenue d'un déverrouillage valide).
    pub fn change_password(&self, key_hex: &str, new_password: &str) -> Result<()> {
        if new_password.len() < 8 {
            return Err(AppError::Invalid(
                "Le mot de passe maître doit faire au moins 8 caractères.".into(),
            ));
        }
        let key = hex::decode(key_hex).map_err(|_| AppError::Security("clé invalide".into()))?;
        let mut meta = self.meta()?;
        meta.pw = wrap(&key, new_password.as_bytes())?;
        self.save_meta(&meta)
    }

    pub fn regenerate_phrase(&self, key_hex: &str) -> Result<String> {
        let key = hex::decode(key_hex).map_err(|_| AppError::Security("clé invalide".into()))?;
        let phrase = generate_phrase();
        let mut meta = self.meta()?;
        meta.rp = wrap(&key, phrase.as_bytes())?;
        self.save_meta(&meta)?;
        Ok(phrase)
    }

    // — Trousseau OS (opt-in) —

    pub fn keychain_store(&self, key_hex: &str) -> Result<()> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
            .map_err(|e| AppError::Security(format!("trousseau : {e}")))?;
        entry
            .set_password(key_hex)
            .map_err(|e| AppError::Security(format!("trousseau : {e}")))?;
        let mut meta = self.meta()?;
        meta.keychain = true;
        self.save_meta(&meta)
    }

    pub fn keychain_load(&self) -> Option<String> {
        let meta = self.meta().ok()?;
        if !meta.keychain {
            return None;
        }
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
            .ok()?
            .get_password()
            .ok()
    }

    pub fn keychain_clear(&self) -> Result<()> {
        if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
            let _ = entry.delete_password();
        }
        let mut meta = self.meta()?;
        meta.keychain = false;
        self.save_meta(&meta)
    }
}

fn derive(secret: &[u8], salt: &[u8]) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    Argon2::default()
        .hash_password_into(secret, salt, &mut out)
        .map_err(|e| AppError::Security(format!("dérivation de clé : {e}")))?;
    Ok(out)
}

fn wrap(key: &[u8], secret: &[u8]) -> Result<WrappedKey> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce);
    let kek = derive(secret, &salt)?;
    let cipher = ChaCha20Poly1305::new((&kek).into());
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), key)
        .map_err(|_| AppError::Security("chiffrement de la clé impossible".into()))?;
    Ok(WrappedKey {
        salt: hex::encode(salt),
        nonce: hex::encode(nonce),
        wrapped: hex::encode(ct),
    })
}

fn unwrap(w: &WrappedKey, secret: &[u8]) -> Result<Vec<u8>> {
    let salt = hex::decode(&w.salt).map_err(|_| AppError::Security("meta corrompu".into()))?;
    let nonce = hex::decode(&w.nonce).map_err(|_| AppError::Security("meta corrompu".into()))?;
    let ct = hex::decode(&w.wrapped).map_err(|_| AppError::Security("meta corrompu".into()))?;
    let kek = derive(secret, &salt)?;
    let cipher = ChaCha20Poly1305::new((&kek).into());
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_slice())
        .map_err(|_| AppError::Security("secret incorrect".into()))
}

fn generate_phrase() -> String {
    let mut rng = rand::thread_rng();
    let mut words = Vec::with_capacity(12);
    for _ in 0..12 {
        let mut b = [0u8; 1];
        rng.fill_bytes(&mut b);
        words.push(WORDLIST[b[0] as usize % WORDLIST.len()]);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_roundtrip() {
        let key = [7u8; 32];
        let w = wrap(&key, b"mot-de-passe-test").unwrap();
        assert_eq!(unwrap(&w, b"mot-de-passe-test").unwrap(), key.to_vec());
        assert!(unwrap(&w, b"mauvais").is_err());
    }
}
