//! Parcours d'acceptation du pipeline progressif : catalogue immédiat, FTS
//! indépendant, enrichissement convergent et recherche sémantique d'une archive.

use std::sync::Arc;
use syn_app::bus::Bus;
use syn_app::connectors::files::{IndexJob, Indexer};
use syn_app::db::Db;
use syn_app::error::{AppError, Result};
use syn_app::llm::{ChatMessage, GenParams, LlmClient, LlmResponse, LlmStatus, ToolSpec};

struct AcceptanceLlm;

#[async_trait::async_trait]
impl LlmClient for AcceptanceLlm {
    async fn generate(
        &self,
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
        _params: GenParams,
    ) -> Result<LlmResponse> {
        Err(AppError::Other("non utilisé".into()))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let folded = syn_app::db::fold(text);
                if folded.contains("securite")
                    || folded.contains("politique")
                    || folded.contains("pssi")
                {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }

    async fn status(&self) -> LlmStatus {
        LlmStatus {
            available: true,
            runtime: "acceptance".into(),
            chat_model_ready: false,
            embed_model_ready: true,
            installed_models: vec![],
            detail: None,
        }
    }

    async fn pull(
        &self,
        _model: &str,
        _progress: tokio::sync::mpsc::Sender<(f32, String)>,
    ) -> Result<()> {
        Ok(())
    }
}

#[test]
fn catalogue_fts_et_enrichissement_progressif_de_bout_en_bout() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    tauri::async_runtime::set(runtime.handle().clone());
    runtime.block_on(async {
        let root =
            std::env::temp_dir().join(format!("syn-progressive-e2e-{}", uuid::Uuid::new_v4()));
        let docs = root.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("Archive_2017.txt"),
            "Politique interne de continuité et sécurité du personnel.",
        )
        .unwrap();
        for index in 0..350 {
            std::fs::write(
                docs.join(format!("note-{index}.txt")),
                format!("Note historique {index}"),
            )
            .unwrap();
        }
        let db = Db::open(&root.join("syn.db"), &"3".repeat(64)).unwrap();
        db.with(|connection| {
            connection.execute(
                "INSERT INTO settings(key,value) VALUES ('embed_model','\"acceptance\"')
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [],
            )?;
            connection.execute(
                "INSERT INTO folders(path,added_at,status) VALUES (?1,0,'active')",
                [docs.to_string_lossy().to_string()],
            )?;
            Ok(())
        })
        .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(AcceptanceLlm);
        let indexer = Indexer::start(db.clone(), llm.clone(), Bus::new(), "acceptance".into());
        indexer
            .tx
            .send(IndexJob::FullScan(Some(docs.clone())))
            .unwrap();

        for _ in 0..200 {
            let status = indexer.status(&db).unwrap();
            if status.catalog_ready && status.eligible_count >= 351 && !status.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let before = indexer.status(&db).unwrap();
        assert!(before.catalog_ready);
        assert!(
            before.eligible_count >= 351,
            "aucun plafond à 300 : {before:?}"
        );
        let started = std::time::Instant::now();
        let lexical = syn_app::retrieval::search_lexical_source(&db, "Archive 2017", 5, "files")
            .await
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(lexical[0].title, "Archive_2017.txt");

        // Les lots répétés représentent les fenêtres idle + secteur et doivent
        // finir par vider toute la file, y compris au-delà du 300e document.
        for _ in 0..15 {
            indexer.tx.send(IndexJob::Drain(32)).unwrap();
        }
        for _ in 0..500 {
            let status = indexer.status(&db).unwrap();
            if status.embedded_count == status.eligible_count && !status.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let after = indexer.status(&db).unwrap();
        assert_eq!(after.embedded_count, after.eligible_count);
        assert_eq!(after.coverage_pct, 100.0);
        let semantic = syn_app::retrieval::search_source(
            &db,
            &llm,
            "document conceptuel sur la PSSI",
            8,
            "files",
        )
        .await
        .unwrap();
        assert!(
            semantic.iter().any(|item| item.title == "Archive_2017.txt"),
            "archive absente des résultats sémantiques : {semantic:#?}"
        );
        indexer.stop_and_wait().await;
        let _ = std::fs::remove_dir_all(root);
    });
}
