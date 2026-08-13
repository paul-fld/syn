use serde::Serialize;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("base de données : {0}")]
    Db(#[from] rusqlite::Error),
    #[error("entrées/sorties : {0}")]
    Io(#[from] std::io::Error),
    #[error("sérialisation : {0}")]
    Json(#[from] serde_json::Error),
    #[error("réseau : {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Locked(String),
    #[error("{0}")]
    Security(String),
    #[error("{0}")]
    Llm(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn locked() -> Self {
        AppError::Locked("Syn est verrouillé : mot de passe maître requis.".into())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Other(e.to_string())
    }
}

// Les commandes Tauri sérialisent l'erreur vers le frontend.
impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct E<'a> {
            kind: &'a str,
            message: String,
        }
        let kind = match self {
            AppError::Locked(_) => "locked",
            AppError::Llm(_) => "llm",
            AppError::Security(_) => "security",
            AppError::NotFound(_) => "not_found",
            AppError::Invalid(_) => "invalid",
            _ => "internal",
        };
        E {
            kind,
            message: self.to_string(),
        }
        .serialize(s)
    }
}
