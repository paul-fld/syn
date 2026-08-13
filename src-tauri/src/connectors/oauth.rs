//! OAuth des connecteurs externes.
//! Les applications desktop sont des clients publics : Google et Microsoft
//! utilisent Authorization Code + PKCE, GitHub utilise le Device Flow.
//! Les jetons sont conservés dans le trousseau du système, jamais dans SQLite.

use crate::connectors;
use crate::db::Db;
use crate::error::{AppError, Result};
use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEYCHAIN_SERVICE: &str = "app.syn.desktop.oauth";

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn client_id(provider: &str) -> Option<String> {
    env(match provider {
        "google" => "SYN_GOOGLE_CLIENT_ID",
        "microsoft" => "SYN_MICROSOFT_CLIENT_ID",
        "github" => "SYN_GITHUB_CLIENT_ID",
        "slack" => "SYN_SLACK_CLIENT_ID",
        _ => return None,
    })
}

pub fn is_configured(provider: &str) -> bool {
    match provider {
        "google" | "microsoft" | "github" => client_id(provider).is_some(),
        "slack" => {
            client_id(provider).is_some()
                && env("SYN_SLACK_CLIENT_SECRET").is_some()
                && env("SYN_SLACK_REDIRECT_URI").is_some()
                && env("SYN_SLACK_CALLBACK_PORT").is_some()
        }
        _ => false,
    }
}

pub fn configuration_detail(provider: &str) -> String {
    if has_token(provider) {
        return "Compte autorisé ; jeton protégé dans le trousseau système.".into();
    }
    if is_configured(provider) {
        return match provider {
            "github" => "Prêt à connecter avec le Device Flow GitHub.".into(),
            "slack" => "Configuration de développement détectée ; callback HTTPS requis.".into(),
            _ => "Configuration de développement détectée ; connexion PKCE prête.".into(),
        };
    }
    let variable = match provider {
        "google" => "SYN_GOOGLE_CLIENT_ID",
        "microsoft" => "SYN_MICROSOFT_CLIENT_ID",
        "github" => "SYN_GITHUB_CLIENT_ID",
        "slack" => "SYN_SLACK_CLIENT_ID, SYN_SLACK_CLIENT_SECRET, SYN_SLACK_REDIRECT_URI et SYN_SLACK_CALLBACK_PORT",
        _ => "identifiants développeur",
    };
    format!("Ajoute {variable} pour activer la connexion de développement.")
}

fn entry(provider: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, provider)
        .map_err(|error| AppError::Other(format!("Trousseau OAuth indisponible : {error}")))
}

pub fn has_token(provider: &str) -> bool {
    entry(provider)
        .and_then(|value| {
            value
                .get_password()
                .map_err(|error| AppError::Other(error.to_string()))
        })
        .is_ok()
}

fn save_token(provider: &str, token: &Value) -> Result<()> {
    entry(provider)?
        .set_password(&token.to_string())
        .map_err(|error| {
            AppError::Other(format!("Impossible de protéger le jeton OAuth : {error}"))
        })
}

pub fn revoke_local(provider: &str) {
    if let Ok(value) = entry(provider) {
        let _ = value.delete_password();
    }
}

pub async fn start(db: &Db, provider: &str) -> Result<Value> {
    if !is_configured(provider) {
        return Err(AppError::Invalid(configuration_detail(provider)));
    }
    match provider {
        "google" | "microsoft" => start_pkce(db.clone(), provider.to_string()).await,
        "github" => start_github_device(db.clone()).await,
        "slack" => start_slack(db.clone()).await,
        _ => Err(AppError::Invalid("fournisseur OAuth inconnu".into())),
    }
}

async fn start_slack(db: Db) -> Result<Value> {
    let id = client_id("slack").unwrap_or_default();
    let secret = env("SYN_SLACK_CLIENT_SECRET").unwrap_or_default();
    let redirect = env("SYN_SLACK_REDIRECT_URI").unwrap_or_default();
    let port = env("SYN_SLACK_CALLBACK_PORT")
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| {
            AppError::Invalid("SYN_SLACK_CALLBACK_PORT doit être un port valide".into())
        })?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| AppError::Other(format!("Callback Slack indisponible : {error}")))?;
    let state = random_hex(24);
    let authorization_url = format!(
        "https://slack.com/oauth/v2/authorize?client_id={}&user_scope={}&redirect_uri={}&state={}",
        encode(&id),
        encode("search:read,channels:history,groups:history,im:history,mpim:history,files:read,users:read"),
        encode(&redirect),
        encode(&state),
    );
    tokio::spawn(async move {
        let result = receive_slack_callback(listener, &id, &secret, &redirect, &state).await;
        if let Ok(token) = result {
            if save_token("slack", &token).is_ok() {
                let _ = connectors::set_status(&db, "slack", "slack", "connected");
            }
        }
    });
    Ok(json!({
        "status":"authorization_required",
        "flow":"https_callback",
        "authorization_url":authorization_url,
        "message":format!("Autorise Slack dans ton navigateur. Le relais HTTPS doit transférer le callback vers 127.0.0.1:{port}.")
    }))
}

async fn receive_slack_callback(
    listener: tokio::net::TcpListener,
    id: &str,
    secret: &str,
    redirect: &str,
    expected_state: &str,
) -> Result<Value> {
    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
            .await
            .map_err(|_| AppError::Other("La connexion Slack a expiré.".into()))?
            .map_err(|error| AppError::Other(format!("Callback Slack invalide : {error}")))?;
    let mut buffer = vec![0u8; 16 * 1024];
    let read = stream
        .read(&mut buffer)
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| AppError::Invalid("retour Slack illisible".into()))?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| AppError::Invalid(format!("retour Slack invalide : {error}")))?;
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let valid = query
        .get("state")
        .is_some_and(|value| value == expected_state);
    let code = query.get("code").cloned();
    let body = if valid && code.is_some() {
        "Slack est autorisé. Tu peux fermer cette page et revenir dans Syn."
    } else {
        "Connexion Slack refusée ou invalide. Reviens dans Syn pour réessayer."
    };
    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(response.as_bytes()).await;
    if !valid {
        return Err(AppError::Security("État OAuth Slack invalide".into()));
    }
    let code = code.ok_or_else(|| AppError::Invalid("autorisation Slack refusée".into()))?;
    let response = reqwest::Client::new()
        .post("https://slack.com/api/oauth.v2.access")
        .form(&[
            ("client_id", id),
            ("client_secret", secret),
            ("code", code.as_str()),
            ("redirect_uri", redirect),
        ])
        .send()
        .await
        .map_err(|error| AppError::Other(format!("Échange Slack impossible : {error}")))?;
    let token: Value = response
        .json()
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;
    if token["ok"] != true {
        return Err(AppError::Other(format!("OAuth Slack refusé : {token}")));
    }
    Ok(token)
}

async fn start_pkce(db: Db, provider: String) -> Result<Value> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| AppError::Other(format!("Callback OAuth indisponible : {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| AppError::Other(error.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
    let state = random_hex(24);
    let verifier = random_hex(48);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let id = client_id(&provider).unwrap_or_default();
    let (authorize, scope, extra) = if provider == "google" {
        (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "openid email profile https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar https://www.googleapis.com/auth/drive.metadata.readonly",
            "&access_type=offline&prompt=consent",
        )
    } else {
        (
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "openid profile email offline_access User.Read Mail.Read Mail.Send Calendars.ReadWrite Files.Read.All",
            "",
        )
    };
    let authorization_url = format!(
        "{authorize}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256{extra}",
        encode(&id), encode(&redirect), encode(scope), encode(&state), encode(&challenge)
    );
    let response_provider = provider.clone();
    tokio::spawn(async move {
        let result =
            receive_pkce_callback(listener, &provider, &id, &redirect, &state, &verifier).await;
        if let Ok(token) = result {
            if save_token(&provider, &token).is_ok() {
                let _ = connectors::set_status(&db, &provider, &provider, "connected");
            }
        }
    });
    Ok(json!({
        "status":"authorization_required",
        "flow":"pkce",
        "provider":response_provider,
        "authorization_url":authorization_url,
        "message":"Autorise le compte dans ton navigateur. Cette fenêtre se mettra à jour automatiquement."
    }))
}

async fn receive_pkce_callback(
    listener: tokio::net::TcpListener,
    provider: &str,
    id: &str,
    redirect: &str,
    expected_state: &str,
    verifier: &str,
) -> Result<Value> {
    let accepted = tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
        .await
        .map_err(|_| AppError::Other("La connexion OAuth a expiré.".into()))?
        .map_err(|error| AppError::Other(format!("Callback OAuth invalide : {error}")))?;
    let (mut stream, _) = accepted;
    let mut buffer = vec![0u8; 16 * 1024];
    let read = stream
        .read(&mut buffer)
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| AppError::Invalid("retour OAuth illisible".into()))?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| AppError::Invalid(format!("retour OAuth invalide : {error}")))?;
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let valid = query
        .get("state")
        .is_some_and(|value| value == expected_state);
    let code = query.get("code").cloned();
    let body = if valid && code.is_some() {
        "Connexion autorisée. Tu peux fermer cette page et revenir dans Syn."
    } else {
        "Connexion refusée ou invalide. Reviens dans Syn pour réessayer."
    };
    let http = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(http.as_bytes()).await;
    if !valid {
        return Err(AppError::Security("État OAuth invalide".into()));
    }
    let code = code.ok_or_else(|| AppError::Invalid("autorisation OAuth refusée".into()))?;
    let token_endpoint = if provider == "google" {
        "https://oauth2.googleapis.com/token"
    } else {
        "https://login.microsoftonline.com/common/oauth2/v2.0/token"
    };
    let response = reqwest::Client::new()
        .post(token_endpoint)
        .form(&[
            ("client_id", id),
            ("code", code.as_str()),
            ("redirect_uri", redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|error| AppError::Other(format!("Échange OAuth impossible : {error}")))?;
    let status = response.status();
    let token: Value = response
        .json()
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;
    if !status.is_success() {
        return Err(AppError::Other(format!("OAuth refusé : {token}")));
    }
    Ok(token)
}

async fn start_github_device(db: Db) -> Result<Value> {
    let id = client_id("github").unwrap_or_default();
    let response = reqwest::Client::new()
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", id.as_str()),
            ("scope", "read:user user:email repo"),
        ])
        .send()
        .await
        .map_err(|error| AppError::Other(format!("GitHub indisponible : {error}")))?;
    let payload: Value = response
        .json()
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;
    let device_code = payload["device_code"]
        .as_str()
        .ok_or_else(|| AppError::Other(payload.to_string()))?
        .to_string();
    let interval = payload["interval"].as_u64().unwrap_or(5);
    let expires = payload["expires_in"].as_u64().unwrap_or(900);
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            let Ok(response) = client
                .post("https://github.com/login/oauth/access_token")
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", id.as_str()),
                    ("device_code", device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await
            else {
                continue;
            };
            let Ok(token) = response.json::<Value>().await else {
                continue;
            };
            if token.get("access_token").is_some() {
                if save_token("github", &token).is_ok() {
                    let _ = connectors::set_status(&db, "github", "github", "connected");
                }
                break;
            }
            if token["error"] != "authorization_pending" && token["error"] != "slow_down" {
                break;
            }
        }
    });
    Ok(json!({
        "status":"authorization_required",
        "flow":"device",
        "authorization_url":payload["verification_uri"],
        "user_code":payload["user_code"],
        "message":format!("Saisis le code {} dans GitHub.", payload["user_code"].as_str().unwrap_or(""))
    }))
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    hex::encode(value)
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
