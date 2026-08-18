//! Connecteurs cloud Google Workspace et Microsoft 365.
//! Les données utiles sont mises en miroir dans l'index local chiffré ; les
//! jetons restent exclusivement dans le trousseau macOS.

use crate::bus::{Bus, BusEvent};
use crate::db::{new_id, now, Db};
use crate::error::{AppError, Result};
use crate::ingestion;
use crate::llm::LlmClient;
use crate::memory::{self, Item};
use base64::Engine;
use futures_util::{stream, StreamExt};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_MESSAGES: usize = 100;
const MAX_FILES: usize = 500;
const MAX_CLOUD_FILE_BYTES: u64 = 20 * 1024 * 1024;

pub async fn sync(
    provider: &str,
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
) -> Result<Value> {
    let has_baseline = match provider {
        "google" => {
            load_cursor(db, "google", "drive_changes")?.is_some()
                && load_cursor(db, "google", "gmail_history")?.is_some()
        }
        "microsoft" => {
            load_cursor(db, "microsoft", "drive_delta")?.is_some()
                && load_cursor(db, "microsoft", "mail_delta")?.is_some()
        }
        _ => return Err(AppError::Invalid("connecteur cloud inconnu".into())),
    };
    if !has_baseline {
        // L'autorisation rend immédiatement les APIs live disponibles. Le
        // premier catalogue complet est volontairement détaché de l'IPC/UI.
        super::set_status(db, provider, provider, "connected")?;
        super::set_diagnostic(
            db,
            provider,
            None,
            Some("Accès immédiat actif · préparation progressive du cache en arrière-plan"),
        )?;
        let provider_owned = provider.to_string();
        let db_owned = db.clone();
        let llm_owned = llm.clone();
        let bus_owned = bus.clone();
        let model_owned = embed_model.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = sync_materialize(
                &provider_owned,
                &db_owned,
                &llm_owned,
                &bus_owned,
                &model_owned,
            )
            .await
            {
                let _ = super::set_diagnostic(
                    &db_owned,
                    &provider_owned,
                    Some(&error.to_string()),
                    Some("Accès API actif · cache de fond à reprendre"),
                );
            }
        });
        return Ok(json!({
            "status":"connected", "preparing":true,
            "mail":0, "files":0, "events":0
        }));
    }
    sync_materialize(provider, db, llm, bus, embed_model).await
}

async fn sync_materialize(
    provider: &str,
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
) -> Result<Value> {
    bus.emit(BusEvent::SyncProgress {
        connector: provider.into(),
        pct: 2.0,
        message: Some(format!("Synchronisation {provider}…")),
    });
    let token = super::oauth::access_token(provider).await?;
    let (mail, files, events) = if provider == "google" {
        progress(
            bus,
            provider,
            8.0,
            "Accès immédiat à Google activé · préparation du cache…",
        );
        tokio::try_join!(
            sync_google_mail(db, llm, bus, embed_model, &token),
            sync_google_drive(db, llm, bus, embed_model, &token),
            sync_google_calendar(db, &token),
        )?
    } else if provider == "microsoft" {
        progress(
            bus,
            provider,
            8.0,
            "Accès immédiat à Microsoft activé · préparation du cache…",
        );
        tokio::try_join!(
            sync_ms_mail(db, llm, bus, embed_model, &token),
            sync_ms_drive(db, llm, bus, embed_model, &token),
            sync_ms_calendar(db, &token),
        )?
    } else {
        return Err(AppError::Invalid("connecteur cloud inconnu".into()));
    };
    super::set_status(db, provider, provider, "connected")?;
    let (full_syncs, delta_syncs) = cursor_counts(db, provider).unwrap_or_default();
    let summary = format!(
        "{mail} mail(s) · {files} fichier(s) · {events} événement(s) · reprises delta {delta_syncs} (initiales {full_syncs})"
    );
    super::set_diagnostic(db, provider, None, Some(&summary))?;
    crate::security::log_access(
        db,
        provider,
        "sync",
        Some(&format!("full={full_syncs};delta={delta_syncs}")),
    );
    bus.emit(BusEvent::SyncProgress {
        connector: provider.into(),
        pct: 100.0,
        message: Some(format!(
            "{provider} synchronisé : {mail} mail(s), {files} fichier(s), {events} événement(s)."
        )),
    });
    Ok(json!({"status":"connected", "mail":mail, "files":files, "events":events}))
}

fn progress(bus: &Bus, provider: &str, pct: f32, message: &str) {
    bus.emit(BusEvent::SyncProgress {
        connector: provider.into(),
        pct,
        message: Some(message.into()),
    });
}

async fn get_json(client: &reqwest::Client, url: &str, token: &str) -> Result<Value> {
    let response = client.get(url).bearer_auth(token).send().await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        return Err(AppError::Other(format!("API cloud {status} : {value}")));
    }
    Ok(value)
}

fn load_cursor(db: &Db, provider: &str, resource: &str) -> Result<Option<String>> {
    db.with(|connection| {
        connection
            .query_row(
                "SELECT cursor FROM connector_cursors WHERE provider=?1 AND resource=?2",
                params![provider, resource],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| {
                if error == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(error.into())
                }
            })
    })
}

fn cursor_counts(db: &Db, provider: &str) -> Result<(i64, i64)> {
    db.with(|connection| {
        connection
            .query_row(
                "SELECT COALESCE(SUM(full_sync_count),0),COALESCE(SUM(delta_sync_count),0)
                 FROM connector_cursors WHERE provider=?1",
                [provider],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
    })
}

fn save_cursor(db: &Db, provider: &str, resource: &str, cursor: &str, delta: bool) -> Result<()> {
    db.with(|connection| {
        connection.execute(
            "INSERT INTO connector_cursors
             (provider,resource,cursor,updated_at,full_sync_count,delta_sync_count)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(provider,resource) DO UPDATE SET
               cursor=excluded.cursor,updated_at=excluded.updated_at,
               full_sync_count=full_sync_count+excluded.full_sync_count,
               delta_sync_count=delta_sync_count+excluded.delta_sync_count",
            params![provider, resource, cursor, now(), !delta, delta],
        )?;
        Ok(())
    })
}

fn remove_remote_item(db: &Db, source_ref: &str) -> Result<()> {
    db.with(|connection| {
        connection.execute(
            "UPDATE items SET status='deleted', ingested_at=?2 WHERE source_ref=?1",
            params![source_ref, now()],
        )?;
        connection.execute(
            "UPDATE enrichment_queue SET state='removed', updated_at=?2 WHERE source_ref=?1",
            params![source_ref, now()],
        )?;
        Ok(())
    })
}

fn timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp())
}

fn microsoft_timestamp(value: &str) -> Option<i64> {
    timestamp(value).or_else(|| {
        chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
            .ok()
            .map(|date| date.and_utc().timestamp())
    })
}

fn microsoft_datetime(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| {
            date.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| value.trim_end_matches('Z').to_string())
}

fn hash(text: &str) -> Option<String> {
    Some(blake3::hash(text.as_bytes()).to_hex().to_string())
}

async fn ingest(
    db: &Db,
    _llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    source: &str,
    source_ref: String,
    kind: &str,
    title: String,
    body: String,
    created_at: Option<i64>,
    path: Option<String>,
    mime: Option<String>,
    size: Option<i64>,
) -> Result<()> {
    let item = Item {
        id: String::new(),
        source: source.into(),
        source_ref,
        r#type: kind.into(),
        title: Some(title),
        body: Some(body.clone()),
        created_at,
        ingested_at: now(),
        hash: hash(&body),
        path,
        mime,
        size,
        mtime: created_at,
        status: "active".into(),
    };
    // Une première synchro peut importer des centaines d'objets. Les stocker et
    // les rendre cherchables par FTS ne doit pas attendre un appel d'embedding
    // par objet : le rattrapage global les vectorise ensuite par lots de 64.
    let title = item.title.clone().unwrap_or_default();
    let source = item.source.clone();
    let (id, changed) = memory::upsert_item(db, &item)?;
    if changed {
        let rows = ingestion::chunk(&body)
            .into_iter()
            .map(|text| (text, None))
            .collect::<Vec<_>>();
        memory::replace_embeddings(db, &id, embed_model, &rows)?;
        bus.emit(BusEvent::ItemIngested {
            item_id: id.clone(),
            source: source.clone(),
            title,
        });
    }
    db.with(|connection| {
        connection.execute(
            "INSERT INTO enrichment_queue
             (item_id,source,source_ref,state,base_priority,lexical_ready,updated_at)
             VALUES (?1,?2,?3,'pending',?4,?5,?6)
             ON CONFLICT(item_id) DO UPDATE SET source_ref=excluded.source_ref,
             base_priority=excluded.base_priority,
             state=CASE WHEN ?7 THEN 'pending'
                        WHEN enrichment_queue.embedding_ready=1 THEN enrichment_queue.state
                        ELSE 'pending' END,
             embedding_ready=CASE WHEN ?7 THEN 0 ELSE enrichment_queue.embedding_ready END,
             lexical_ready=MAX(enrichment_queue.lexical_ready,excluded.lexical_ready),
             updated_at=excluded.updated_at",
            params![
                id,
                source,
                item.source_ref,
                if item.source == "mail" { 700.0 } else { 450.0 },
                !body.trim().is_empty(),
                now(),
                changed
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}

fn gmail_header(payload: &Value, name: &str) -> String {
    payload["headers"]
        .as_array()
        .and_then(|headers| {
            headers.iter().find(|header| {
                header["name"]
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|header| header["value"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn gmail_body(part: &Value) -> String {
    let mime = part["mimeType"].as_str().unwrap_or("");
    if let Some(data) = part["body"]["data"].as_str() {
        if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if mime == "text/html" {
                return strip_html(&text);
            }
            if mime.starts_with("text/") || mime.is_empty() {
                return text;
            }
        }
    }
    part["parts"]
        .as_array()
        .map(|parts| parts.iter().map(gmail_body).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn strip_html(input: &str) -> String {
    let mut output = String::new();
    let mut tag = false;
    for character in input.chars() {
        match character {
            '<' => tag = true,
            '>' => {
                tag = false;
                output.push(' ');
            }
            _ if !tag => output.push(character),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn sync_google_mail(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    token: &str,
) -> Result<usize> {
    let client = reqwest::Client::new();
    let previous = load_cursor(db, "google", "gmail_history")?;
    let mut ids = Vec::new();
    let mut removed = Vec::new();
    let mut next_history = None;
    let mut delta = previous.is_some();

    if let Some(history_id) = previous.as_deref() {
        let mut page = None;
        loop {
            let mut url = format!("https://gmail.googleapis.com/gmail/v1/users/me/history?startHistoryId={history_id}&historyTypes=messageAdded&historyTypes=messageDeleted&maxResults=500");
            if let Some(value) = page.as_deref() {
                url.push_str("&pageToken=");
                url.push_str(&urlencoding(value));
            }
            match get_json(&client, &url, token).await {
                Ok(value) => {
                    for history in value["history"].as_array().cloned().unwrap_or_default() {
                        ids.extend(
                            history["messagesAdded"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|entry| {
                                    entry["message"]["id"].as_str().map(str::to_string)
                                }),
                        );
                        removed.extend(
                            history["messagesDeleted"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|entry| {
                                    entry["message"]["id"].as_str().map(str::to_string)
                                }),
                        );
                    }
                    next_history = value["historyId"].as_str().map(str::to_string);
                    page = value["nextPageToken"].as_str().map(str::to_string);
                    if page.is_none() {
                        break;
                    }
                }
                Err(error) if error.to_string().contains("404") => {
                    // Gmail invalide les historyId trop anciens. C'est le seul cas
                    // où une reliste complète est nécessaire.
                    delta = false;
                    ids.clear();
                    break;
                }
                Err(error) => return Err(error),
            }
        }
    }

    if !delta {
        // Capture le watermark avant la reliste : un message arrivé pendant la
        // pagination sera repris au prochain history.list, jamais perdu.
        let profile = get_json(
            &client,
            "https://gmail.googleapis.com/gmail/v1/users/me/profile",
            token,
        )
        .await?;
        next_history = profile["historyId"].as_str().map(str::to_string);
        let mut page = None;
        loop {
            let mut url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults={MAX_MESSAGES}"
            );
            if let Some(value) = page.as_deref() {
                url.push_str("&pageToken=");
                url.push_str(&urlencoding(value));
            }
            let value = get_json(&client, &url, token).await?;
            ids.extend(
                value["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|message| message["id"].as_str().map(str::to_string)),
            );
            page = value["nextPageToken"].as_str().map(str::to_string);
            if page.is_none() {
                break;
            }
        }
    }
    for id in removed {
        remove_remote_item(db, &format!("google:gmail:{id}"))?;
    }
    ids.sort();
    ids.dedup();
    let requests = ids.into_iter();
    let fetched = stream::iter(requests)
        .map(|id| {
            let client = client.clone();
            async move {
                let value = get_json(
                    &client,
                    &format!(
                        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
                    ),
                    token,
                )
                .await?;
                Ok::<_, AppError>((id, value))
            }
        })
        // Première hydratation volontairement douce : l'UI et les recherches
        // gardent la priorité sur le débit de téléchargement Gmail.
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let mut count = 0;
    let total = fetched.len().max(1);
    for (index, result) in fetched.into_iter().enumerate() {
        if index % 10 == 0 {
            progress(
                bus,
                "google",
                8.0 + 32.0 * index as f32 / total as f32,
                &format!("Gmail : {index}/{total} messages…"),
            );
        }
        let (id, value) = result?;
        let payload = &value["payload"];
        let subject = gmail_header(payload, "Subject");
        let from = gmail_header(payload, "From");
        let to = gmail_header(payload, "To");
        let date = gmail_header(payload, "Date");
        let content = gmail_body(payload);
        let body = format!("De : {from}\nÀ : {to}\nDate : {date}\nObjet : {subject}\n\n{content}");
        let created = value["internalDate"]
            .as_str()
            .and_then(|v| v.parse::<i64>().ok())
            .map(|v| v / 1000);
        ingest(
            db,
            llm,
            bus,
            embed_model,
            "mail",
            format!("google:gmail:{id}"),
            "email",
            if subject.is_empty() {
                "(sans objet)".into()
            } else {
                subject
            },
            body,
            created,
            Some(format!("https://mail.google.com/mail/u/0/#all/{id}")),
            Some("message/rfc822".into()),
            None,
        )
        .await?;
        count += 1;
    }
    if let Some(cursor) = next_history {
        save_cursor(db, "google", "gmail_history", &cursor, delta)?;
    }
    Ok(count)
}

async fn sync_google_drive(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    token: &str,
) -> Result<usize> {
    let client = reqwest::Client::new();
    let previous = load_cursor(db, "google", "drive_changes")?;
    let delta = previous.is_some();
    let mut files = Vec::new();
    let next_cursor;
    if let Some(mut page) = previous {
        loop {
            let fields = urlencoding("nextPageToken,newStartPageToken,changes(removed,fileId,file(id,name,mimeType,modifiedTime,webViewLink,size,description,trashed))");
            let value = get_json(&client, &format!("https://www.googleapis.com/drive/v3/changes?pageToken={}&pageSize={MAX_FILES}&includeRemoved=true&fields={fields}", urlencoding(&page)), token).await?;
            for change in value["changes"].as_array().cloned().unwrap_or_default() {
                let id = change["fileId"].as_str().unwrap_or_default();
                if change["removed"].as_bool().unwrap_or(false)
                    || change["file"]["trashed"].as_bool().unwrap_or(false)
                {
                    remove_remote_item(db, &format!("google:drive:{id}"))?;
                } else if change["file"].is_object() {
                    files.push(change["file"].clone());
                }
            }
            if let Some(next) = value["nextPageToken"].as_str() {
                page = next.to_string();
            } else {
                next_cursor = value["newStartPageToken"]
                    .as_str()
                    .unwrap_or(&page)
                    .to_string();
                break;
            }
        }
    } else {
        let start = get_json(
            &client,
            "https://www.googleapis.com/drive/v3/changes/startPageToken",
            token,
        )
        .await?;
        next_cursor = start["startPageToken"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut page = None;
        loop {
            let fields = urlencoding("nextPageToken,files(id,name,mimeType,modifiedTime,webViewLink,size,description,trashed)");
            let mut url = format!("https://www.googleapis.com/drive/v3/files?pageSize={MAX_FILES}&q=trashed%3Dfalse&fields={fields}");
            if let Some(value) = page.as_deref() {
                url.push_str("&pageToken=");
                url.push_str(&urlencoding(value));
            }
            let value = get_json(&client, &url, token).await?;
            files.extend(value["files"].as_array().cloned().unwrap_or_default());
            page = value["nextPageToken"].as_str().map(str::to_string);
            if page.is_none() {
                break;
            }
        }
    }
    let mut count = 0;
    let total = files.len().max(1);
    for (index, file) in files.into_iter().enumerate() {
        if index % 10 == 0 {
            progress(
                bus,
                "google",
                42.0 + 44.0 * index as f32 / total as f32,
                &format!("Google Drive : {index}/{total} fichiers…"),
            );
        }
        let Some(id) = file["id"].as_str() else {
            continue;
        };
        let name = file["name"]
            .as_str()
            .unwrap_or("Document sans nom")
            .to_string();
        let mime = file["mimeType"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string();
        if mime == "application/vnd.google-apps.folder" {
            continue;
        }
        let url = file["webViewLink"].as_str().map(str::to_string);
        let modified = file["modifiedTime"].as_str().and_then(timestamp);
        let description = file["description"].as_str().unwrap_or("");
        // Le catalogue Drive est immédiatement recherchable par nom, type et
        // description. Télécharger/extracter des dizaines de fichiers ici
        // recréerait une synchronisation bloquante.
        let content: Option<String> = None;
        let body = format!("Nom : {name}\nType : {mime}\nDescription : {description}\nEmplacement cloud : Google Drive\n\n{}", content.unwrap_or_default());
        ingest(
            db,
            llm,
            bus,
            embed_model,
            "cloud",
            format!("google:drive:{id}"),
            "document",
            name,
            body,
            modified,
            url,
            Some(mime),
            file["size"].as_str().and_then(|v| v.parse().ok()),
        )
        .await?;
        count += 1;
    }
    if !next_cursor.is_empty() {
        save_cursor(db, "google", "drive_changes", &next_cursor, delta)?;
    }
    Ok(count)
}

async fn sync_google_calendar(db: &Db, token: &str) -> Result<usize> {
    let client = reqwest::Client::new();
    let min = chrono::Utc::now() - chrono::Duration::days(30);
    let max = chrono::Utc::now() + chrono::Duration::days(365);
    let url = format!("https://www.googleapis.com/calendar/v3/calendars/primary/events?singleEvents=true&orderBy=startTime&maxResults=500&timeMin={}&timeMax={}",
        urlencoding(&min.to_rfc3339()), urlencoding(&max.to_rfc3339()));
    let value = get_json(&client, &url, token).await?;
    let events = value["items"].as_array().cloned().unwrap_or_default();
    for event in &events {
        let Some(remote_id) = event["id"].as_str() else {
            continue;
        };
        let start = event["start"]["dateTime"]
            .as_str()
            .and_then(timestamp)
            .or_else(|| {
                event["start"]["date"].as_str().and_then(|v| {
                    format!("{v}T00:00:00Z")
                        .as_str()
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .ok()
                        .map(|d| d.timestamp())
                })
            })
            .unwrap_or(0);
        let end = event["end"]["dateTime"].as_str().and_then(timestamp);
        db.with(|c| { c.execute(
            "INSERT INTO events (id,source,source_ref,title,\"start\",\"end\",location,attendees,notes) VALUES (?1,'google',?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(source_ref) DO UPDATE SET title=excluded.title,\"start\"=excluded.\"start\",\"end\"=excluded.\"end\",location=excluded.location,attendees=excluded.attendees,notes=excluded.notes",
            params![new_id(), format!("google:calendar:{remote_id}"), event["summary"].as_str().unwrap_or("(sans titre)"), start, end, event["location"].as_str(), event["attendees"].to_string(), event["description"].as_str()])?; Ok(()) })?;
    }
    Ok(events.len())
}

async fn sync_ms_mail(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    token: &str,
) -> Result<usize> {
    let client = reqwest::Client::new();
    let previous = load_cursor(db, "microsoft", "mail_delta")?;
    let delta = previous.is_some();
    let mut url = previous.unwrap_or_else(|| format!("https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$top={MAX_MESSAGES}&$select=id,subject,from,toRecipients,receivedDateTime,bodyPreview,body,webLink"));
    let mut messages = Vec::new();
    let cursor;
    loop {
        let response = client
            .get(&url)
            .bearer_auth(token)
            .header("Prefer", "outlook.body-content-type=\"text\"")
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            return Err(AppError::Other(format!(
                "Microsoft Graph {status} : {value}"
            )));
        }
        for message in value["value"].as_array().cloned().unwrap_or_default() {
            if message["@removed"].is_object() {
                if let Some(id) = message["id"].as_str() {
                    remove_remote_item(db, &format!("microsoft:mail:{id}"))?;
                }
            } else {
                messages.push(message);
            }
        }
        if let Some(next) = value["@odata.nextLink"].as_str() {
            url = next.to_string();
        } else {
            cursor = value["@odata.deltaLink"]
                .as_str()
                .unwrap_or(&url)
                .to_string();
            break;
        }
    }
    for message in &messages {
        let Some(id) = message["id"].as_str() else {
            continue;
        };
        let subject = message["subject"]
            .as_str()
            .unwrap_or("(sans objet)")
            .to_string();
        let from = message["from"]["emailAddress"]["address"]
            .as_str()
            .unwrap_or("");
        let recipients = message["toRecipients"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v["emailAddress"]["address"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let content = message["body"]["content"]
            .as_str()
            .unwrap_or_else(|| message["bodyPreview"].as_str().unwrap_or(""));
        let body = format!("De : {from}\nÀ : {recipients}\nObjet : {subject}\n\n{content}");
        ingest(
            db,
            llm,
            bus,
            embed_model,
            "mail",
            format!("microsoft:mail:{id}"),
            "email",
            subject,
            body,
            message["receivedDateTime"].as_str().and_then(timestamp),
            message["webLink"].as_str().map(str::to_string),
            Some("message/rfc822".into()),
            None,
        )
        .await?;
    }
    save_cursor(db, "microsoft", "mail_delta", &cursor, delta)?;
    Ok(messages.len())
}

async fn sync_ms_drive(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    token: &str,
) -> Result<usize> {
    let client = reqwest::Client::new();
    let previous = load_cursor(db, "microsoft", "drive_delta")?;
    let delta = previous.is_some();
    let mut url = previous.unwrap_or_else(|| format!("https://graph.microsoft.com/v1.0/me/drive/root/delta?$top={MAX_FILES}&$select=id,name,file,folder,size,lastModifiedDateTime,webUrl,deleted"));
    let mut files = Vec::new();
    let cursor;
    loop {
        let value = get_json(&client, &url, token).await?;
        files.extend(value["value"].as_array().cloned().unwrap_or_default());
        if let Some(next) = value["@odata.nextLink"].as_str() {
            url = next.to_string();
        } else {
            cursor = value["@odata.deltaLink"]
                .as_str()
                .unwrap_or(&url)
                .to_string();
            break;
        }
    }
    let mut count = 0;
    let total = files.len().max(1);
    for (index, file) in files.into_iter().enumerate() {
        if index % 10 == 0 {
            progress(
                bus,
                "microsoft",
                42.0 + 44.0 * index as f32 / total as f32,
                &format!("OneDrive : {index}/{total} fichiers…"),
            );
        }
        if file["deleted"].is_object() {
            if let Some(id) = file["id"].as_str() {
                remove_remote_item(db, &format!("microsoft:drive:{id}"))?;
            }
            continue;
        }
        if file["folder"].is_object() {
            continue;
        }
        let Some(id) = file["id"].as_str() else {
            continue;
        };
        let name = file["name"]
            .as_str()
            .unwrap_or("Document sans nom")
            .to_string();
        let mime = file["file"]["mimeType"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string();
        let content: Option<String> = None;
        let body = format!(
            "Nom : {name}\nType : {mime}\nEmplacement cloud : Microsoft OneDrive\n\n{}",
            content.unwrap_or_default()
        );
        ingest(
            db,
            llm,
            bus,
            embed_model,
            "cloud",
            format!("microsoft:drive:{id}"),
            "document",
            name,
            body,
            file["lastModifiedDateTime"].as_str().and_then(timestamp),
            file["webUrl"].as_str().map(str::to_string),
            Some(mime),
            file["size"].as_i64(),
        )
        .await?;
        count += 1;
    }
    save_cursor(db, "microsoft", "drive_delta", &cursor, delta)?;
    Ok(count)
}

async fn sync_ms_calendar(db: &Db, token: &str) -> Result<usize> {
    let client = reqwest::Client::new();
    let start = urlencoding(&(chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339());
    let end = urlencoding(&(chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339());
    let value = get_json(&client, &format!("https://graph.microsoft.com/v1.0/me/calendarView?startDateTime={start}&endDateTime={end}&$top=500&$select=id,subject,start,end,location,attendees,bodyPreview,webLink"), token).await?;
    let events = value["value"].as_array().cloned().unwrap_or_default();
    for event in &events {
        let Some(remote_id) = event["id"].as_str() else {
            continue;
        };
        let start = event["start"]["dateTime"]
            .as_str()
            .and_then(microsoft_timestamp)
            .unwrap_or(0);
        let end = event["end"]["dateTime"]
            .as_str()
            .and_then(microsoft_timestamp);
        db.with(|c| { c.execute(
            "INSERT INTO events (id,source,source_ref,title,\"start\",\"end\",location,attendees,notes) VALUES (?1,'microsoft',?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(source_ref) DO UPDATE SET title=excluded.title,\"start\"=excluded.\"start\",\"end\"=excluded.\"end\",location=excluded.location,attendees=excluded.attendees,notes=excluded.notes",
            params![new_id(),format!("microsoft:calendar:{remote_id}"),event["subject"].as_str().unwrap_or("(sans titre)"),start,end,event["location"]["displayName"].as_str(),event["attendees"].to_string(),event["bodyPreview"].as_str()])?; Ok(()) })?;
    }
    Ok(events.len())
}

/// Enrichissement sémantique appelé exclusivement par le worker de fond. Le
/// chemin de recherche ne télécharge, n'extrait et ne vectorise jamais ici.
pub async fn enrich_item(
    source: &str,
    item_id: &str,
    source_ref: &str,
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    _bus: &Bus,
    embed_model: &str,
) -> Result<()> {
    let (title, existing_body, mime, size): (String, String, String, Option<u64>) =
        db.with(|c| {
            c.query_row(
                "SELECT COALESCE(title,''),COALESCE(body,''),COALESCE(mime,''),size
             FROM items WHERE id=?1 AND status='active'",
                [item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, Option<i64>>(3)?
                            .map(|value| value.max(0) as u64),
                    ))
                },
            )
            .map_err(Into::into)
        })?;
    let body = if source == "cloud" {
        let mut parts = source_ref.splitn(3, ':');
        let provider = parts.next().unwrap_or_default();
        let _kind = parts.next();
        let remote_id = parts
            .next()
            .ok_or_else(|| AppError::Invalid("référence cloud incomplète".into()))?;
        let token = super::oauth::access_token(provider).await?;
        let content = download_cloud_text(provider, remote_id, &title, &mime, size, &token)
            .await
            .unwrap_or_default();
        if content.trim().is_empty() {
            existing_body
        } else {
            format!("{existing_body}\n\n{content}")
        }
    } else {
        existing_body
    };
    let chunks = ingestion::chunk(&body);
    if chunks.is_empty() {
        return Ok(());
    }
    db.with(|c| {
        c.execute(
            "UPDATE items SET body=?2,hash=?3,ingested_at=?4 WHERE id=?1",
            params![item_id, body, hash(&body), now()],
        )?;
        Ok(())
    })?;
    let lexical = chunks
        .iter()
        .cloned()
        .map(|text| (text, None))
        .collect::<Vec<_>>();
    memory::replace_embeddings(db, item_id, embed_model, &lexical)?;
    let vectors = llm.embed(&chunks).await?;
    let rows = chunks
        .into_iter()
        .zip(vectors.into_iter())
        .map(|(text, vector)| (text, Some(crate::llm::vec_to_blob(&vector))))
        .collect::<Vec<_>>();
    memory::replace_embeddings(db, item_id, embed_model, &rows)?;
    Ok(())
}

async fn download_cloud_text(
    provider: &str,
    id: &str,
    name: &str,
    mime: &str,
    size: Option<u64>,
    token: &str,
) -> Option<String> {
    if size.is_some_and(|value| value > MAX_CLOUD_FILE_BYTES) {
        return None;
    }
    let client = reqwest::Client::new();
    let url = if provider == "google" {
        match mime {
            "application/vnd.google-apps.document" => format!(
                "https://www.googleapis.com/drive/v3/files/{id}/export?mimeType=text%2Fplain"
            ),
            "application/vnd.google-apps.spreadsheet" => {
                format!("https://www.googleapis.com/drive/v3/files/{id}/export?mimeType=text%2Fcsv")
            }
            value if value.starts_with("application/vnd.google-apps.") => return None,
            _ => format!("https://www.googleapis.com/drive/v3/files/{id}?alt=media"),
        }
    } else {
        format!("https://graph.microsoft.com/v1.0/me/drive/items/{id}/content")
    };
    let response = client.get(url).bearer_auth(token).send().await.ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|value| value > MAX_CLOUD_FILE_BYTES)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if provider == "google" && mime.starts_with("application/vnd.google-apps.") {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let safe_name = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = std::env::temp_dir().join(format!("syn-cloud-{}-{safe_name}", uuid::Uuid::new_v4()));
    std::fs::write(&path, &bytes).ok()?;
    let extracted = crate::ingestion::extract::extract(&path).text;
    let _ = std::fs::remove_file(path);
    extracted
}

/// Recherche directement chez le fournisseur. Le cache améliore la mémoire et
/// le hors-ligne, mais il n'est jamais un prérequis pour répondre à une
/// recherche interactive.
pub async fn live_search(kind: &str, query: &str) -> Vec<Value> {
    let mut results = Vec::new();
    for provider in ["google", "microsoft"] {
        if let Ok(mut found) = live_search_provider(kind, query, provider).await {
            results.append(&mut found);
        }
    }
    results.truncate(20);
    results
}

/// Variante stricte utilisée lorsqu'une requête nomme explicitement Google ou
/// Microsoft. Elle ne doit jamais basculer silencieusement vers l'autre compte.
pub async fn live_search_provider(kind: &str, query: &str, provider: &str) -> Result<Vec<Value>> {
    if !super::oauth::has_token(provider) {
        return Err(AppError::NotFound(format!(
            "Le compte {provider} n'est pas connecté."
        )));
    }
    let token = super::oauth::access_token(provider).await?;
    match (provider, kind) {
        ("google", "mail") => live_google_mail(query, &token).await,
        ("google", "cloud") => live_google_drive(query, &token).await,
        ("microsoft", "mail") => live_ms_mail(query, &token).await,
        ("microsoft", "cloud") => live_ms_drive(query, &token).await,
        _ => Err(AppError::Invalid(format!(
            "recherche {kind} non prise en charge pour {provider}"
        ))),
    }
}

async fn live_google_mail(query: &str, token: &str) -> Result<Vec<Value>> {
    let client = reqwest::Client::new();
    // Gmail cherche TOUS les mots de `q` : lui passer la phrase de l'utilisateur
    // ne ramenait jamais rien. On interroge avec les mots porteurs, puis on
    // élargit au plus distinctif si la recherche stricte ne donne rien.
    let terms = search_terms(query);
    let mut tentatives = Vec::new();
    if !terms.is_empty() {
        tentatives.push(terms.join(" "));
        if terms.len() > 1 {
            tentatives.push(format!("{{{}}}", terms.join(" ")));
        }
    }
    if tentatives.is_empty() {
        tentatives.push(query.trim().to_string());
    }
    let mut list = Value::Null;
    for tentative in &tentatives {
        list = get_json(
            &client,
            &format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10&q={}",
                urlencoding(tentative)
            ),
            token,
        )
        .await?;
        if list["messages"]
            .as_array()
            .is_some_and(|found| !found.is_empty())
        {
            break;
        }
    }
    let requests = list["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|message| message["id"].as_str().map(str::to_string));
    let fetched = stream::iter(requests)
        .map(|id| {
            let client = client.clone();
            async move {
                let value = get_json(
                    &client,
                    &format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=Subject&metadataHeaders=From&metadataHeaders=Date"),
                    token,
                )
                .await?;
                Ok::<_, AppError>((id, value))
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;
    let mut out = Vec::new();
    for result in fetched {
        let (id, value) = result?;
        out.push(json!({
            "item_id": format!("live:google:gmail:{id}"),
            "source": "mail",
            "source_ref": format!("google:gmail:{id}"),
            "title": gmail_header(&value["payload"], "Subject"),
            "path": format!("https://mail.google.com/mail/u/0/#all/{id}"),
            "snippet": value["snippet"].as_str().unwrap_or_default(),
            "from": gmail_header(&value["payload"], "From"),
            "date": gmail_header(&value["payload"], "Date"),
            "provider": "google",
            "live": true,
        }));
    }
    Ok(out)
}

async fn live_google_drive(query: &str, token: &str) -> Result<Vec<Value>> {
    let client = reqwest::Client::new();
    let fields = urlencoding("files(id,name,mimeType,modifiedTime,webViewLink,description,size)");
    let terms = search_terms(query);
    // Les filtres vont du plus précis au plus large. On s'arrête au premier qui
    // répond : élargir alors qu'une expression exacte a déjà donné un résultat
    // ne ferait que noyer le bon document.
    let mut files = Vec::new();
    for filter in google_drive_filters(query) {
        let value = get_json(
            &client,
            &format!(
                "https://www.googleapis.com/drive/v3/files?pageSize=25&corpora=allDrives\
                 &includeItemsFromAllDrives=true&supportsAllDrives=true&spaces=drive&q={}&fields={fields}",
                urlencoding(&filter)
            ),
            token,
        )
        .await?;
        files = value["files"].as_array().cloned().unwrap_or_default();
        if !files.is_empty() {
            break;
        }
    }
    let mut out = files
        .into_iter()
        .filter_map(|file| {
            let id = file["id"].as_str()?.to_string();
            let name = file["name"].as_str().unwrap_or_default();
            Some(json!({
                "item_id": format!("live:google:drive:{id}"),
                "source": "cloud",
                "source_ref": format!("google:drive:{id}"),
                "title": file["name"],
                "path": file["webViewLink"],
                "snippet": file["description"].as_str().unwrap_or("Correspondance Google Drive"),
                "provider": "google",
                "score": cloud_match_score(name, query, &terms),
                "live": true,
            }))
        })
        .collect::<Vec<_>>();
    sort_by_score(&mut out);
    out.truncate(10);
    Ok(out)
}

use crate::retrieval::{is_connective, is_request_filler};

/// Bruit de formulation : mot de requête (« ressortir », « google ») ou simple
/// liaison (« dans », « pour »). Ni l'un ni l'autre n'a sa place dans une
/// clause `fullText contains` obligatoire.
fn is_noise(word: &str) -> bool {
    is_request_filler(word) || is_connective(word)
}

/// Mots porteurs de sens d'une demande documentaire. La phrase qui les entoure
/// (« le document du … qui se trouve dans mes Google Docs ») ne doit jamais
/// atteindre le fournisseur : elle ramène tout le Drive.
///
/// Deux subtilités payées comptant : un mot composé (« montre-moi ») doit être
/// jugé sur ses parties, sinon il devient un terme obligatoire introuvable ; et
/// un token court qui contient un chiffre (« Q3 », « T2 ») est au contraire le
/// plus discriminant de la demande — la limite de trois caractères ne s'y
/// applique pas.
/// Les mots porteurs d'une demande, pour interroger un fournisseur. Une phrase
/// entière passée à Gmail (« Tu peux me retrouver un mail de Liverpool qui
/// concerne ma réservation… ») ne ramène rien : l'API cherche TOUS les mots.
pub fn query_terms(query: &str) -> Vec<String> {
    search_terms(query)
}

fn search_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.trim_matches(|character: char| !character.is_alphanumeric()))
        .filter(|term| {
            let folded = crate::db::fold(term);
            let has_digit = folded.chars().any(|character| character.is_ascii_digit());
            if folded.chars().count() < if has_digit { 2 } else { 3 } {
                return false;
            }
            // « montre-moi » = « montre » + « moi » : deux mots de formulation,
            // donc rien à chercher. « vade_mecum » ou « compte-rendu » gardent
            // au moins une partie porteuse et survivent.
            folded
                .split(['-', '\'', '’'])
                .any(|part| part.chars().count() >= 2 && !is_noise(part))
        })
        .take(6)
        .map(str::to_string)
        .collect()
}

/// Échappe une valeur destinée à une chaîne entre apostrophes d'un filtre Drive.
/// Les guillemets sont retirés : Drive ne sait pas les échapper à l'intérieur
/// d'une expression exacte.
fn drive_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', " ")
        .trim()
        .to_string()
}

/// Filtres Drive successifs, du plus précis au plus large. Le connecteur s'arrête
/// au premier qui répond.
///
/// Deux pièges de l'API dictent cette construction. D'une part `name contains`
/// ne fait qu'une correspondance de préfixe : `name contains 'Vie'` ne trouve
/// pas « Le Jeu de la Vie ». D'autre part `fullText contains 'Jeu de la Vie'`
/// sans guillemets internes est interprété mot à mot, si bien que « de » et
/// « la » ramènent l'intégralité d'un Drive francophone.
///
/// D'où trois paliers : l'expression exacte (quand la demande ressemble à un
/// titre), la conjonction des termes porteurs, puis leur disjonction. Ce dernier
/// palier existe parce qu'un seul terme mal choisi suffit à rendre une
/// conjonction vide ; le classement par titre fait redescendre le bruit qu'il
/// laisse passer.
fn google_drive_filters(query: &str) -> Vec<String> {
    let mut filters = Vec::new();
    let phrase = drive_literal(query);
    // Une phrase entière (« Tu peux me ressortir le document du … ? ») n'est le
    // titre d'aucun fichier : chercher l'expression exacte coûterait un
    // aller-retour pour rien.
    let looks_like_a_title = !phrase.is_empty() && phrase.split_whitespace().count() <= 8;
    if looks_like_a_title {
        filters.push(format!(
            "trashed=false and (fullText contains '\"{phrase}\"' or name contains '{phrase}')"
        ));
    }
    let terms = search_terms(query)
        .iter()
        .map(|term| drive_literal(term))
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let clauses = |joiner: &str| {
        terms
            .iter()
            .map(|term| format!("fullText contains '\"{term}\"'"))
            .collect::<Vec<_>>()
            .join(joiner)
    };
    if terms.len() > 1 {
        filters.push(format!("trashed=false and ({})", clauses(" and ")));
        filters.push(format!("trashed=false and ({})", clauses(" or ")));
    } else if terms.len() == 1 {
        filters.push(format!("trashed=false and ({})", clauses(" and ")));
    }
    if filters.is_empty() {
        // Demande sans aucun mot exploitable : on interroge quand même le titre
        // brut plutôt que de renvoyer tout le Drive.
        filters.push(if phrase.is_empty() {
            "trashed=false and name contains ''".into()
        } else {
            format!(
                "trashed=false and (fullText contains '\"{phrase}\"' or name contains '{phrase}')"
            )
        });
    }
    filters
}

/// Rang d'un résultat distant. Le titre prime : c'est ce que l'utilisateur a en
/// tête quand il réclame « le document du Jeu de la Vie ».
fn cloud_match_score(title: &str, query: &str, terms: &[String]) -> f64 {
    let folded_title = crate::db::fold(title);
    let folded_query = crate::db::fold(query);
    let mut score = 4.0;
    if !folded_query.is_empty() && folded_title.contains(&folded_query) {
        score += 6.0;
    }
    if !terms.is_empty() {
        let hits = terms
            .iter()
            .filter(|term| folded_title.contains(&crate::db::fold(term)))
            .count();
        score += 4.0 * hits as f64 / terms.len() as f64;
    }
    score
}

fn sort_by_score(values: &mut [Value]) {
    values.sort_by(|left, right| {
        right["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&left["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

async fn live_ms_mail(query: &str, token: &str) -> Result<Vec<Value>> {
    let client = reqwest::Client::new();
    // Même raison que pour Gmail : la phrase entière ne correspond à rien.
    let terms = search_terms(query);
    let keywords = if terms.is_empty() {
        query.trim().to_string()
    } else {
        terms.join(" ")
    };
    let search = urlencoding(&format!("\"{keywords}\""));
    let value = get_json(
        &client,
        &format!("https://graph.microsoft.com/v1.0/me/messages?$search={search}&$top=10&$select=id,subject,from,receivedDateTime,bodyPreview,webLink"),
        token,
    )
    .await?;
    Ok(value["value"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mail| {
            let id = mail["id"].as_str()?.to_string();
            Some(json!({
                "item_id": format!("live:microsoft:mail:{id}"),
                "source": "mail",
                "source_ref": format!("microsoft:mail:{id}"),
                "title": mail["subject"],
                "path": mail["webLink"],
                "snippet": mail["bodyPreview"],
                "from": mail["from"]["emailAddress"]["name"]
                    .as_str()
                    .or_else(|| mail["from"]["emailAddress"]["address"].as_str())
                    .unwrap_or_default(),
                "date": mail["receivedDateTime"],
                "provider": "microsoft",
                "live": true,
            }))
        })
        .collect())
}

async fn live_ms_drive(query: &str, token: &str) -> Result<Vec<Value>> {
    let client = reqwest::Client::new();
    let terms = search_terms(query);
    // Graph indexe par mots-clés : lui envoyer la phrase entière fait remonter
    // les documents qui partagent seulement « dans », « mes » ou « document ».
    let keywords = if terms.is_empty() {
        query.trim().to_string()
    } else {
        terms.join(" ")
    };

    let mut files = Vec::new();
    let own_drive = get_json(
        &client,
        &format!(
            "https://graph.microsoft.com/v1.0/me/drive/root/search(q='{}')?$top=25&$select=id,name,file,size,lastModifiedDateTime,webUrl",
            urlencoding(&keywords.replace('\'', "''"))
        ),
        token,
    )
    .await;
    let own_error = own_drive.as_ref().err().map(ToString::to_string);
    if let Ok(value) = own_drive {
        files.extend(value["value"].as_array().cloned().unwrap_or_default());
    }
    // `/me/drive` s'arrête au OneDrive personnel. Word, Excel, PowerPoint et
    // les fichiers SharePoint partagés ne sont atteignables que par l'API de
    // recherche Microsoft, qui couvre l'ensemble des emplacements du compte.
    let shared = ms_search_drive_items(&client, &keywords, token).await;
    if let Ok(values) = &shared {
        files.extend(values.clone());
    }
    if files.is_empty() {
        if let Some(error) = own_error.or_else(|| shared.err().map(|e| e.to_string())) {
            return Err(AppError::Other(error));
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = files
        .into_iter()
        .filter_map(|file| {
            let id = file["id"].as_str()?.to_string();
            if !seen.insert(id.clone()) {
                return None;
            }
            let name = file["name"].as_str().unwrap_or_default();
            Some(json!({
                "item_id": format!("live:microsoft:drive:{id}"),
                "source": "cloud",
                "source_ref": format!("microsoft:drive:{id}"),
                "title": file["name"],
                "path": file["webUrl"],
                "snippet": "Correspondance OneDrive",
                "provider": "microsoft",
                "score": cloud_match_score(name, query, &terms),
                "live": true,
            }))
        })
        .collect::<Vec<_>>();
    sort_by_score(&mut out);
    out.truncate(10);
    Ok(out)
}

/// Recherche transverse Microsoft (OneDrive, partages, SharePoint). Exige le
/// consentement `Sites.Read.All` en plus de `Files.Read.All`.
async fn ms_search_drive_items(
    client: &reqwest::Client,
    keywords: &str,
    token: &str,
) -> Result<Vec<Value>> {
    let body = json!({
        "requests": [{
            "entityTypes": ["driveItem"],
            "query": {"queryString": keywords},
            "from": 0,
            "size": 25,
        }]
    });
    let response = client
        .post("https://graph.microsoft.com/v1.0/search/query")
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        return Err(AppError::Other(format!("API cloud {status} : {value}")));
    }
    Ok(value["value"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|response| {
            response["hitsContainers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|container| container["hits"].as_array().cloned().unwrap_or_default())
        .filter_map(|hit| {
            let resource = hit["resource"].clone();
            resource["id"].as_str()?;
            Some(resource)
        })
        .collect())
}

/// Mémorise un résultat obtenu en direct chez le fournisseur. Sans cela, un
/// document trouvé par la recherche live reste absent du cache : il n'est pas
/// ouvrable (la garde de périmètre ne le connaît pas), son contenu n'est jamais
/// téléchargé, et la même question reposée hors ligne ne trouve plus rien.
pub async fn remember_live_result(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    value: &Value,
) -> Result<()> {
    let Some(source_ref) = value["source_ref"].as_str() else {
        return Ok(());
    };
    let is_mail = value["source"].as_str() == Some("mail");
    let title = value["title"]
        .as_str()
        .unwrap_or(if is_mail { "Message" } else { "Document cloud" })
        .to_string();
    let microsoft = source_ref.starts_with("microsoft:");
    let snippet = value["snippet"].as_str().unwrap_or_default();
    // Un mail retrouvé en direct doit devenir un item connu de Syn : c'est ce
    // qui rend son lien ouvrable (la garde de périmètre n'ouvre que ce que Syn
    // connaît) et ce qui le laisse retrouvable hors ligne.
    let (source, kind, body) = if is_mail {
        let boite = if microsoft { "Outlook" } else { "Gmail" };
        (
            "mail",
            "email",
            format!("Objet : {title}\nMessagerie : {boite}\n\n{snippet}"),
        )
    } else {
        let emplacement = if microsoft {
            "OneDrive"
        } else {
            "Google Drive"
        };
        (
            "cloud",
            "document",
            format!("Nom : {title}\nEmplacement cloud : {emplacement}\n\n{snippet}"),
        )
    };
    ingest(
        db,
        llm,
        bus,
        embed_model,
        source,
        source_ref.to_string(),
        kind,
        title,
        body,
        None,
        value["path"].as_str().map(str::to_string),
        None,
        None,
    )
    .await
}

/// Crée un vrai document chez le fournisseur : un Google Doc natif côté Google,
/// un fichier Word côté Microsoft. Exige les portées d'écriture (`drive.file`,
/// `Files.ReadWrite.All`) — une autorisation obtenue avant leur ajout doit être
/// renouvelée depuis Réglages ▸ Connecteurs.
pub async fn create_document(provider: &str, title: &str, content: &str) -> Result<Value> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    if provider == "google" {
        let boundary = format!("syn-{}", uuid::Uuid::new_v4());
        let metadata = json!({
            "name": title,
            "mimeType": "application/vnd.google-apps.document",
        });
        let body = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n\
             --{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{content}\r\n\
             --{boundary}--"
        );
        let response = client
            .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&supportsAllDrives=true&fields=id,name,webViewLink")
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/related; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            return Err(AppError::Other(format!(
                "API Google Drive {status} : {value}"
            )));
        }
        let id = value["id"].as_str().unwrap_or_default().to_string();
        return Ok(json!({
            "provider": "google",
            "service": "Google Docs",
            "id": id,
            "name": value["name"],
            "url": value["webViewLink"],
            "source_ref": format!("google:drive:{id}"),
        }));
    }

    // Microsoft : on crée l'entrée puis on y verse le paquet Word. Passer par
    // l'identifiant évite d'avoir à échapper un chemin dans l'URL.
    let name = format!("{}.docx", crate::tools::documents::safe_file_name(title));
    let created = client
        .post("https://graph.microsoft.com/v1.0/me/drive/root/children")
        .bearer_auth(&token)
        .json(&json!({
            "name": name,
            "file": {},
            "@microsoft.graph.conflictBehavior": "rename",
        }))
        .send()
        .await?;
    let status = created.status();
    let item: Value = created.json().await?;
    if !status.is_success() {
        return Err(AppError::Other(format!(
            "API Microsoft Graph {status} : {item}"
        )));
    }
    let id = item["id"]
        .as_str()
        .ok_or_else(|| AppError::Other("Graph n'a pas renvoyé d'identifiant".into()))?
        .to_string();
    let bytes = crate::tools::documents::docx_bytes(content)?;
    let uploaded = client
        .put(format!(
            "https://graph.microsoft.com/v1.0/me/drive/items/{id}/content"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .body(bytes)
        .send()
        .await?;
    let status = uploaded.status();
    let final_item: Value = uploaded.json().await?;
    if !status.is_success() {
        return Err(AppError::Other(format!(
            "API Microsoft Graph {status} : {final_item}"
        )));
    }
    Ok(json!({
        "provider": "microsoft",
        "service": "OneDrive (Word)",
        "id": id,
        "name": final_item["name"].as_str().unwrap_or(&name),
        "url": final_item["webUrl"].as_str().or_else(|| item["webUrl"].as_str()),
        "source_ref": format!("microsoft:drive:{id}"),
    }))
}

/// Le type MIME d'un fichier Drive : c'est lui qui dit si c'est un Doc, une
/// feuille ou une présentation, et donc quelle API sait le modifier.
pub async fn drive_mime(file_id: &str) -> Result<String> {
    let token = super::oauth::access_token("google").await?;
    let client = reqwest::Client::new();
    let value = get_json(
        &client,
        &format!("https://www.googleapis.com/drive/v3/files/{file_id}?fields=mimeType,name&supportsAllDrives=true"),
        &token,
    )
    .await?;
    Ok(value["mimeType"].as_str().unwrap_or_default().to_string())
}

/// Les pièces jointes d'un message, téléchargées sur la machine pour que Syn
/// puisse les lire comme n'importe quel document.
pub async fn download_attachments(
    provider: &str,
    message_id: &str,
) -> Result<Vec<std::path::PathBuf>> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    let dossier = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| AppError::Other("dossier de téléchargement introuvable".into()))?
        .join("Syn — pièces jointes");
    std::fs::create_dir_all(&dossier)
        .map_err(|error| AppError::Other(format!("dossier impossible à créer : {error}")))?;

    let mut fichiers = Vec::new();
    if provider == "google" {
        let message = get_json(
            &client,
            &format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}?format=full"
            ),
            &token,
        )
        .await?;
        for (nom, attachment_id) in gmail_attachment_parts(&message["payload"]) {
            let part = get_json(
                &client,
                &format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"),
                &token,
            )
            .await?;
            let data = part["data"].as_str().unwrap_or_default();
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(
                    data.replace('-', "+")
                        .replace('_', "/")
                        .trim_end_matches('='),
                )
                .or_else(|_| base64::engine::general_purpose::STANDARD.decode(data))
                .map_err(|_| AppError::Other("pièce jointe illisible".into()))?;
            let chemin =
                crate::tools::reorganize::unique_destination(dossier.join(safe_name(&nom)));
            std::fs::write(&chemin, &bytes)
                .map_err(|error| AppError::Other(format!("écriture impossible : {error}")))?;
            fichiers.push(chemin);
        }
        return Ok(fichiers);
    }

    let liste = get_json(
        &client,
        &format!("https://graph.microsoft.com/v1.0/me/messages/{message_id}/attachments"),
        &token,
    )
    .await?;
    for attachment in liste["value"].as_array().cloned().unwrap_or_default() {
        let Some(nom) = attachment["name"].as_str() else {
            continue;
        };
        let Some(data) = attachment["contentBytes"].as_str() else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| AppError::Other("pièce jointe illisible".into()))?;
        let chemin = crate::tools::reorganize::unique_destination(dossier.join(safe_name(nom)));
        std::fs::write(&chemin, &bytes)
            .map_err(|error| AppError::Other(format!("écriture impossible : {error}")))?;
        fichiers.push(chemin);
    }
    Ok(fichiers)
}

/// Parcourt l'arbre des parties d'un message Gmail et rend (nom, identifiant)
/// pour chaque pièce jointe réelle.
fn gmail_attachment_parts(payload: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pile = vec![payload.clone()];
    while let Some(part) = pile.pop() {
        if let Some(parts) = part["parts"].as_array() {
            pile.extend(parts.iter().cloned());
        }
        let nom = part["filename"].as_str().unwrap_or_default();
        let id = part["body"]["attachmentId"].as_str().unwrap_or_default();
        if !nom.is_empty() && !id.is_empty() {
            out.push((nom.to_string(), id.to_string()));
        }
    }
    out
}

/// Un nom de fichier venu d'un mail est du contenu non fiable : il ne doit pas
/// pouvoir désigner un autre dossier.
fn safe_name(nom: &str) -> String {
    let base = nom.rsplit(['/', '\\']).next().unwrap_or("piece-jointe");
    let nettoye: String = base
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    if nettoye.trim().is_empty() || nettoye.starts_with('.') {
        format!("piece-jointe{nettoye}")
    } else {
        nettoye
    }
}

/// Les derniers messages d'une boîte, sans requête de recherche.
///
/// « Montre-moi mes derniers mails » n'est pas une recherche : il n'y a rien à
/// chercher. Passer par la recherche obligeait à inventer des mots-clés, et le
/// fournisseur répondait à côté ou ne répondait rien.
pub async fn list_mail(provider: &str, unread_only: bool, limit: usize) -> Result<Vec<Value>> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    let limit = limit.clamp(1, 25);
    if provider == "google" {
        let query = if unread_only { "is:unread" } else { "in:inbox" };
        let list = get_json(
            &client,
            &format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults={limit}&q={}",
                urlencoding(query)
            ),
            &token,
        )
        .await?;
        let ids = list["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|message| message["id"].as_str().map(str::to_string));
        let fetched = stream::iter(ids)
            .map(|id| {
                let client = client.clone();
                let token = token.clone();
                async move {
                    let value = get_json(
                        &client,
                        &format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=Subject&metadataHeaders=From&metadataHeaders=Date"),
                        &token,
                    )
                    .await?;
                    Ok::<_, AppError>((id, value))
                }
            })
            .buffer_unordered(10)
            .collect::<Vec<_>>()
            .await;
        let mut out = Vec::new();
        for result in fetched {
            let (id, value) = result?;
            out.push(gmail_summary(&id, &value));
        }
        return Ok(out);
    }
    let filtre = if unread_only {
        "&$filter=isRead%20eq%20false"
    } else {
        ""
    };
    let value = get_json(
        &client,
        &format!("https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages?$top={limit}&$orderby=receivedDateTime%20desc&$select=id,subject,from,receivedDateTime,bodyPreview,webLink,hasAttachments{filtre}"),
        &token,
    )
    .await?;
    Ok(value["value"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(ms_summary)
        .collect())
}

/// Le contenu complet d'un message, pour l'afficher dans la conversation.
pub async fn read_mail(provider: &str, id: &str) -> Result<Value> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    if provider == "google" {
        let value = get_json(
            &client,
            &format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"),
            &token,
        )
        .await?;
        let mut resume = gmail_summary(id, &value);
        resume["body"] = json!(gmail_body(&value["payload"]));
        return Ok(resume);
    }
    let value = get_json(
        &client,
        &format!("https://graph.microsoft.com/v1.0/me/messages/{id}?$select=id,subject,from,receivedDateTime,body,webLink,hasAttachments"),
        &token,
    )
    .await?;
    let mut resume = ms_summary(&value).unwrap_or_else(|| json!({}));
    resume["body"] = json!(strip_html(
        value["body"]["content"].as_str().unwrap_or_default()
    ));
    Ok(resume)
}

/// Met un message à la corbeille. Jamais une suppression définitive : le
/// message reste récupérable chez le fournisseur pendant 30 jours.
pub async fn trash_mail(provider: &str, id: &str) -> Result<Value> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    let response = if provider == "google" {
        client
            .post(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}/trash"
            ))
            .bearer_auth(&token)
            .header("content-length", "0")
            .send()
            .await?
    } else {
        client
            .post(format!(
                "https://graph.microsoft.com/v1.0/me/messages/{id}/move"
            ))
            .bearer_auth(&token)
            .json(&json!({"destinationId": "deleteditems"}))
            .send()
            .await?
    };
    if !response.status().is_success() {
        // Une autorisation obtenue avant l'ajout des portées d'écriture ne
        // permet pas ce geste : le dire, plutôt que de renvoyer le refus brut
        // du fournisseur.
        if response.status() == reqwest::StatusCode::FORBIDDEN
            || response.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(AppError::Security(format!(
                "Ce compte {provider} n'autorise pas encore Syn à déplacer un message. Reconnecte-le depuis Connecteurs pour accorder cette permission."
            )));
        }
        return Err(AppError::Other(format!(
            "Mise à la corbeille refusée par {provider} : {}",
            response.text().await.unwrap_or_default()
        )));
    }
    Ok(json!({"status": "corbeille", "provider": provider, "id": id}))
}

fn gmail_summary(id: &str, value: &Value) -> Value {
    json!({
        "item_id": format!("live:google:gmail:{id}"),
        "source": "mail",
        "source_ref": format!("google:mail:{id}"),
        "title": gmail_header(&value["payload"], "Subject"),
        "path": format!("https://mail.google.com/mail/u/0/#all/{id}"),
        "snippet": value["snippet"].as_str().unwrap_or_default(),
        "from": gmail_header(&value["payload"], "From"),
        "date": gmail_header(&value["payload"], "Date"),
        "provider": "google",
        "live": true,
    })
}

fn ms_summary(mail: &Value) -> Option<Value> {
    let id = mail["id"].as_str()?.to_string();
    Some(json!({
        "item_id": format!("live:microsoft:mail:{id}"),
        "source": "mail",
        "source_ref": format!("microsoft:mail:{id}"),
        "title": mail["subject"],
        "path": mail["webLink"],
        "snippet": mail["bodyPreview"],
        "from": mail["from"]["emailAddress"]["name"]
            .as_str()
            .or_else(|| mail["from"]["emailAddress"]["address"].as_str())
            .unwrap_or_default(),
        "date": mail["receivedDateTime"],
        "attachments": mail["hasAttachments"],
        "provider": "microsoft",
        "live": true,
    }))
}

pub async fn send_mail(provider: &str, to: &str, subject: &str, body: &str) -> Result<Value> {
    let token = super::oauth::access_token(provider).await?;
    let client = reqwest::Client::new();
    let response = if provider == "google" {
        let raw = format!("To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}");
        client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(token)
            .json(&json!({"raw":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)}))
            .send()
            .await?
    } else {
        client.post("https://graph.microsoft.com/v1.0/me/sendMail").bearer_auth(token)
            .json(&json!({"message":{"subject":subject,"body":{"contentType":"Text","content":body},"toRecipients":[{"emailAddress":{"address":to}}]},"saveToSentItems":true})).send().await?
    };
    if !response.status().is_success() {
        return Err(AppError::Other(format!(
            "Envoi {provider} refusé : {}",
            response.text().await.unwrap_or_default()
        )));
    }
    Ok(json!({"status":"envoyé","via":provider,"to":to,"subject":subject}))
}

pub async fn create_event(provider: &str, args: &Value) -> Result<Value> {
    let token = super::oauth::access_token(provider).await?;
    let title = args["title"]
        .as_str()
        .ok_or_else(|| AppError::Invalid("titre requis".into()))?;
    let start = args["start"]
        .as_str()
        .ok_or_else(|| AppError::Invalid("début requis".into()))?;
    let end = args["end"].as_str().unwrap_or(start);
    let attendees = args["attendees"].as_array().cloned().unwrap_or_default();
    let client = reqwest::Client::new();
    let response = if provider == "google" {
        client.post("https://www.googleapis.com/calendar/v3/calendars/primary/events").bearer_auth(token).json(&json!({
            "summary":title,"start":{"dateTime":start},"end":{"dateTime":end},"location":args["location"],
            "attendees":attendees.iter().filter_map(Value::as_str).map(|email|json!({"email":email})).collect::<Vec<_>>()
        })).send().await?
    } else {
        let start = microsoft_datetime(start);
        let end = microsoft_datetime(end);
        client.post("https://graph.microsoft.com/v1.0/me/events").bearer_auth(token).json(&json!({
            "subject":title,"start":{"dateTime":start,"timeZone":"UTC"},"end":{"dateTime":end,"timeZone":"UTC"},
            "location":{"displayName":args["location"].as_str().unwrap_or("")},
            "attendees":attendees.iter().filter_map(Value::as_str).map(|email|json!({"emailAddress":{"address":email},"type":"required"})).collect::<Vec<_>>()
        })).send().await?
    };
    let status = response.status();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(AppError::Other(format!(
            "Création d’événement {provider} refusée : {value}"
        )));
    }
    Ok(json!({"status":"événement créé","via":provider,"event":value}))
}

fn urlencoding(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_un_corps_gmail_et_nettoie_le_html() {
        let encoded =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("<p>Bonjour <b>Paul</b></p>");
        let payload = json!({"mimeType":"text/html","body":{"data":encoded}});
        assert_eq!(gmail_body(&payload), "Bonjour Paul");
    }

    #[test]
    fn comprend_les_dates_graph_sans_suffixe() {
        assert_eq!(
            microsoft_timestamp("2026-08-16T17:30:00.0000000"),
            Some(1_786_901_400)
        );
        assert_eq!(
            microsoft_datetime("2026-08-16T19:30:00+02:00"),
            "2026-08-16T17:30:00"
        );
    }

    #[test]
    fn les_curseurs_cloud_survivent_et_comptent_les_deltas() {
        let root = std::env::temp_dir().join(format!("syn-cursors-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = Db::open(&root.join("test.db"), &"2".repeat(64)).unwrap();
        save_cursor(&db, "google", "drive_changes", "page-1", false).unwrap();
        assert_eq!(
            load_cursor(&db, "google", "drive_changes")
                .unwrap()
                .as_deref(),
            Some("page-1")
        );
        save_cursor(&db, "google", "drive_changes", "page-2", true).unwrap();
        assert_eq!(
            load_cursor(&db, "google", "drive_changes")
                .unwrap()
                .as_deref(),
            Some("page-2")
        );
        let counts: (i64, i64) = db
            .with(|connection| {
                connection
                    .query_row(
                        "SELECT full_sync_count,delta_sync_count FROM connector_cursors
                 WHERE provider='google' AND resource='drive_changes'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            counts,
            (1, 1),
            "un redémarrage doit reprendre page-1 et incrémenter uniquement delta_sync_count"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn la_recherche_drive_interroge_le_titre_par_expression_exacte() {
        let filters = google_drive_filters("Jeu de la Vie");
        assert!(
            filters[0].contains("fullText contains '\"Jeu de la Vie\"'"),
            "{filters:?}"
        );
        // `name contains 'Vie'` ne matche qu'un préfixe côté Drive : il ne doit
        // plus être la seule prise sur « Le Jeu de la Vie 2.0 ».
        assert!(
            !filters
                .iter()
                .any(|filter| filter.contains("contains 'Vie'")),
            "{filters:?}"
        );
        // Le repli conjonctif reste borné aux mots porteurs de sens.
        assert!(
            filters[1].contains("fullText contains '\"Jeu\"' and fullText contains '\"Vie\"'"),
            "{filters:?}"
        );
        assert!(
            !filters.iter().any(|filter| filter.contains("\"la\"")),
            "{filters:?}"
        );
    }

    #[test]
    fn la_phrase_qui_entoure_la_demande_natteint_pas_le_fournisseur() {
        let terms = search_terms(
            "Tu peux me ressortir le document du Jeu de la Vie qui se trouve dans mes Google Docs ?",
        );
        assert_eq!(terms, vec!["Jeu", "Vie"], "{terms:?}");
    }

    #[test]
    fn le_titre_le_plus_proche_passe_devant() {
        let terms = search_terms("Jeu de la Vie");
        let bon = cloud_match_score("Le Jeu de la Vie 2.0 - Sources", "Jeu de la Vie", &terms);
        let hors_sujet = cloud_match_score("Voyage en martinique", "Jeu de la Vie", &terms);
        assert!(bon > hors_sujet, "{bon} devrait dépasser {hors_sujet}");
    }
    /// Garde de généricité : la correction ne doit rien devoir au document de
    /// test. Aucune de ces demandes ne partage de vocabulaire avec les autres,
    /// et deux d'entre elles ne sont pas en français.
    #[test]
    fn les_termes_retenus_sont_ceux_du_document_pas_ceux_de_la_demande() {
        let cas: [(&str, &[&str], &[&str]); 5] = [
            (
                "Tu peux me ressortir le document du Jeu de la Vie qui se trouve dans mes Google Docs ?",
                &["Jeu", "Vie"],
                &["peux", "ressortir", "document", "Google", "Docs"],
            ),
            (
                "cherche le rapport de stage de Maxime dans mon OneDrive",
                &["rapport", "stage", "Maxime"],
                &["cherche", "OneDrive"],
            ),
            (
                "montre-moi le tableur Excel du budget prévisionnel 2027",
                &["tableur", "budget", "prévisionnel", "2027"],
                &["montre-moi", "Excel"],
            ),
            (
                "Where is the Q3 revenue forecast spreadsheet?",
                &["Q3", "revenue", "forecast"],
                &["Where", "the", "spreadsheet"],
            ),
            (
                "ouvre le vade_mecum des sections européennes",
                &["vade_mecum", "sections", "européennes"],
                &["ouvre", "des"],
            ),
        ];
        for (demande, attendus, exclus) in cas {
            let termes = search_terms(demande);
            for attendu in attendus {
                assert!(
                    termes.iter().any(|term| term == attendu),
                    "« {attendu} » manque pour « {demande} » : {termes:?}"
                );
            }
            for exclu in exclus {
                assert!(
                    !termes.iter().any(|term| term == exclu),
                    "« {exclu} » décrit la demande, pas le document : {termes:?}"
                );
            }
        }
    }

    /// La liste des mots de formulation est finie ; la langue ne l'est pas.
    /// Cette garde vérifie qu'un verbe qu'elle NE CONNAÎT PAS (« dégote ») ne
    /// rend pas le document introuvable : la conjonction échoue, la disjonction
    /// rattrape, et le classement par titre remet le bon document en tête.
    #[test]
    fn un_mot_de_formulation_inconnu_ne_rend_pas_le_document_introuvable() {
        let demande = "dégote-moi la convention collective Syntec";
        let termes = search_terms(demande);
        assert!(
            termes.iter().any(|term| term.contains("dégote")),
            "le mot inconnu passe bien à travers le filtre : {termes:?}"
        );

        let filtres = google_drive_filters(demande);
        let dernier = filtres.last().unwrap();
        assert!(
            dernier.contains(" or "),
            "le dernier palier doit élargir, pas exiger : {filtres:?}"
        );
        assert!(
            dernier.contains("'\"convention\"'") && dernier.contains("'\"Syntec\"'"),
            "{filtres:?}"
        );

        // Ce palier laisse passer du bruit — c'est le classement qui tranche.
        let bon = cloud_match_score("Convention collective Syntec 2026.pdf", demande, &termes);
        let bruit = cloud_match_score("Note de service — convention de stage", demande, &termes);
        assert!(
            bon > bruit,
            "le titre le plus proche doit rester premier ({bon} vs {bruit})"
        );
    }

    /// Une conjonction vide ne doit pas être un cul-de-sac : le dernier palier
    /// élargit en disjonction, et le classement par titre trie le bruit.
    #[test]
    fn les_paliers_de_filtre_vont_du_precis_au_large() {
        // Demande courte : elle peut être le titre, on tente l'expression exacte.
        let filtres = google_drive_filters("rapport de stage de Maxime");
        assert_eq!(filtres.len(), 3, "{filtres:?}");
        assert!(
            filtres[0].contains("fullText contains '\"rapport de stage de Maxime\"'"),
            "{filtres:?}"
        );
        assert!(filtres[1].contains("'\"rapport\"' and "), "{filtres:?}");
        assert!(filtres[2].contains("'\"rapport\"' or "), "{filtres:?}");

        // Demande longue : l'expression exacte est sautée, on part des termes.
        let bavarde =
            google_drive_filters("cherche le rapport de stage de Maxime dans mon OneDrive");
        assert_eq!(bavarde.len(), 2, "{bavarde:?}");
        assert!(bavarde[0].contains("'\"rapport\"' and "), "{bavarde:?}");

        // Une phrase de neuf mots ou plus n'est le titre d'aucun fichier :
        // l'aller-retour « expression exacte » est inutile, on ne le fait pas.
        let bavard = google_drive_filters(
            "Tu peux me ressortir le document du Jeu de la Vie qui se trouve dans mes Google Docs ?",
        );
        assert!(
            !bavard[0].contains("Tu peux me ressortir"),
            "la phrase entière ne doit pas être cherchée comme titre : {bavard:?}"
        );
        assert!(
            bavard[0].contains("'\"Jeu\"' and fullText contains '\"Vie\"'"),
            "{bavard:?}"
        );
    }
}
