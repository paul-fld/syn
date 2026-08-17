//! Test d'AUDIT (ajouté par l'auditeur, ne modifie aucun code de production).
//!
//! Cas critique : un document ANCIEN, jamais ouvert, au nom anodin (« REF-4471 »),
//! dont le contenu parle d'un sujet que l'utilisateur formulera autrement (« PSSI »).
//! Il est délibérément placé en DERNIÈRE position de la file de priorité (mtime
//! 2017) derrière 360 fichiers récents, donc au-delà du 300e élément.
//!
//! Trois propriétés, vérifiées dans cet ordre :
//!   (a) trouvable par NOM immédiatement, sans extraction ni embedding ;
//!   (b) NON trouvable sémantiquement à froid (aucun vecteur, corps non extrait) ;
//!   (c) trouvable sémantiquement APRÈS passage de l'enrichissement de fond.

use std::sync::Arc;
use syn_app::bus::Bus;
use syn_app::connectors::files::{IndexJob, Indexer};
use syn_app::db::Db;
use syn_app::error::{AppError, Result};
use syn_app::llm::{ChatMessage, GenParams, LlmClient, LlmResponse, LlmStatus, ToolSpec};

/// Embedder déterministe : projette sur [1,0] tout texte parlant de politique
/// de sécurité — y compris l'acronyme « PSSI », qui ne partage AUCUN mot avec
/// le document. La seule voie possible entre la requête et le document est donc
/// la voie vectorielle.
struct AuditEmbedder;

#[async_trait::async_trait]
impl LlmClient for AuditEmbedder {
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
            runtime: "audit".into(),
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

const SEMANTIC_QUERY: &str = "Peux-tu retrouver le document sur la PSSI de mon entreprise ?";
const TARGET: &str = "REF-4471.txt";

/// `tauri::async_runtime::set` n'accepte qu'une initialisation par processus :
/// les deux tests partagent donc le même runtime.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        tauri::async_runtime::set(runtime.handle().clone());
        runtime
    })
}

fn count(db: &Db, sql: &str) -> i64 {
    db.with(|connection| Ok(connection.query_row(sql, [], |row| row.get(0))?))
        .unwrap()
}

#[test]
fn un_document_ancien_au_nom_anodin_devient_semantique_apres_lenrichissement() {
    runtime().block_on(async {
        // Racine dont le chemin ne contient ni « ref » ni « 4471 », afin que le
        // bruit ne puisse pas fabriquer une correspondance lexicale.
        let root = loop {
            let candidate =
                std::env::temp_dir().join(format!("syn-audit-{}", uuid::Uuid::new_v4()));
            let folded = syn_app::db::fold(&candidate.to_string_lossy());
            if !folded.contains("ref") && !folded.contains("4471") {
                break candidate;
            }
        };
        let docs = root.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();

        // 360 documents récents : ils occupent le haut de la file de priorité.
        for index in 0..360 {
            std::fs::write(
                docs.join(format!("note-{index}.txt")),
                format!("Compte rendu courant numero {index}"),
            )
            .unwrap();
        }
        // La cible : nom anodin, vocabulaire du contenu différent de la requête.
        let target_path = docs.join(TARGET);
        std::fs::write(
            &target_path,
            "Politique interne de securite des systemes d'information applicable au personnel.",
        )
        .unwrap();
        // Ancien : mtime 2017 → score de récence quasi nul → dernier de la file.
        assert!(std::process::Command::new("touch")
            .args(["-t", "201701010101"])
            .arg(&target_path)
            .status()
            .unwrap()
            .success());

        let db = Db::open(&root.join("syn.db"), &"7".repeat(64)).unwrap();
        db.with(|connection| {
            connection.execute(
                "INSERT INTO settings(key,value) VALUES ('embed_model','\"audit\"')
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

        let llm: Arc<dyn LlmClient> = Arc::new(AuditEmbedder);
        let indexer = Indexer::start(db.clone(), llm.clone(), Bus::new(), "audit".into());
        indexer
            .tx
            .send(IndexJob::FullScan(Some(docs.clone())))
            .unwrap();

        for _ in 0..400 {
            let status = indexer.status(&db).unwrap();
            if status.catalog_ready && status.eligible_count >= 361 && !status.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let cold = indexer.status(&db).unwrap();
        assert!(cold.catalog_ready, "catalogue non prêt : {cold:?}");
        assert!(
            cold.eligible_count >= 361,
            "la file n'a pas absorbé tout le corpus : {cold:?}"
        );

        // ————— (a) trouvable par NOM, immédiatement —————
        let started = std::time::Instant::now();
        let by_name = syn_app::retrieval::search_lexical_source(&db, "REF-4471", 5, "files")
            .await
            .unwrap();
        let lexical_latency = started.elapsed();
        assert!(
            lexical_latency < std::time::Duration::from_secs(2),
            "recherche par nom trop lente : {lexical_latency:?}"
        );
        assert_eq!(
            by_name.first().map(|item| item.title.as_str()),
            Some(TARGET),
            "le document n'est pas trouvable par son nom à froid : {by_name:#?}"
        );

        // Preuve que rien n'a été extrait ni vectorisé pour y parvenir.
        let body_is_null: bool = db
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT body IS NULL FROM items WHERE title=?1",
                    [TARGET],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(body_is_null, "le corps a été extrait pendant le catalogue");
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM embeddings WHERE vector IS NOT NULL"
            ),
            0,
            "des vecteurs existent avant tout enrichissement"
        );
        assert_eq!(cold.embedded_count, 0, "couverture non nulle à froid");

        // ————— (b) NON trouvable sémantiquement à froid —————
        let cold_semantic =
            syn_app::retrieval::search_source(&db, &llm, SEMANTIC_QUERY, 8, "files")
                .await
                .unwrap();
        assert!(
            !cold_semantic.iter().any(|item| item.title == TARGET),
            "le document remonte sémantiquement AVANT enrichissement : {cold_semantic:#?}"
        );

        // ————— (c) enrichissement de fond, puis recherche sémantique —————
        // Ces lots sont exactement ceux qu'émet la boucle de fond
        // (lifecycle.rs : IndexJob::Drain(32) par fenêtre idle + secteur).
        for _ in 0..20 {
            indexer.tx.send(IndexJob::Drain(32)).unwrap();
        }
        for _ in 0..900 {
            let status = indexer.status(&db).unwrap();
            if status.embedded_count >= 361 && !status.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let hot = indexer.status(&db).unwrap();
        assert!(
            hot.embedded_count >= 361,
            "la file n'a pas drainé au-delà du 300e élément : {hot:?}"
        );
        assert!(
            hot.embedded_count > cold.embedded_count,
            "le compteur de couverture ne croît pas : {} → {}",
            cold.embedded_count,
            hot.embedded_count
        );
        assert!(
            count(&db, "SELECT COUNT(*) FROM index_metric_log") >= 2,
            "aucun journal de couverture"
        );

        let hot_semantic = syn_app::retrieval::search_source(&db, &llm, SEMANTIC_QUERY, 8, "files")
            .await
            .unwrap();
        assert!(
            hot_semantic.iter().any(|item| item.title == TARGET),
            "le document reste introuvable sémantiquement après enrichissement : {hot_semantic:#?}"
        );

        indexer.stop_and_wait().await;
        let _ = std::fs::remove_dir_all(root);
    });
}

/// Un fichier Drive/OneDrive est catalogué avec un corps de MÉTADONNÉES seules
/// (« Nom : … Type : … Description : … »), donc avec des chunks à vecteur NULL.
/// `backfill_embeddings` vectorisait ces chunks puis marquait la ligne de file
/// `state='embedded'` — état terminal. `enrich_item`, seul endroit qui télécharge
/// le contenu réel du fichier cloud, n'était donc JAMAIS appelé.
///
/// Non-régression : le rattrapage vectorise, mais laisse la ligne cloud dans la
/// file tant que son contenu n'a pas été téléchargé.
#[test]
fn le_backfill_ne_sort_pas_un_fichier_cloud_de_la_file_avant_son_contenu() {
    runtime().block_on(async {
        let root = std::env::temp_dir().join(format!("syn-audit-cloud-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = Db::open(&root.join("syn.db"), &"5".repeat(64)).unwrap();

        // État exact produit par connectors::external::ingest() pour un fichier
        // Google Drive : corps = métadonnées, chunk sans vecteur, file 'pending'.
        let metadata_body = "Nom : Rapport.docx\nType : application/vnd.openxmlformats\n\
                             Description : \nEmplacement cloud : Google Drive\n\n";
        db.with(|connection| {
            connection.execute(
                "INSERT INTO settings(key,value) VALUES ('embed_model','\"audit\"')",
                [],
            )?;
            connection.execute(
                "INSERT INTO items(id,source,source_ref,type,title,body,ingested_at,status)
                 VALUES ('drive-1','cloud','google:drive:abc','document','Rapport.docx',?1,0,'active')",
                [metadata_body],
            )?;
            connection.execute(
                "INSERT INTO embeddings(item_id,model,chunk_index,text,vector)
                 VALUES ('drive-1','audit',0,?1,NULL)",
                [metadata_body],
            )?;
            connection.execute(
                "INSERT INTO enrichment_queue(item_id,source,source_ref,state,base_priority,
                 lexical_ready,updated_at) VALUES ('drive-1','cloud','google:drive:abc',
                 'pending',450,1,0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let llm: Arc<dyn LlmClient> = Arc::new(AuditEmbedder);
        // Exactement l'appel de la boucle de fond (lifecycle.rs:119).
        syn_app::ingestion::backfill_embeddings(&db, &llm, 64)
            .await
            .unwrap();

        let state: String = db
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT state FROM enrichment_queue WHERE item_id='drive-1'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            state, "pending",
            "un objet cloud reste à enrichir tant que son contenu n'est pas téléchargé"
        );
        // Même prédicat que next_enrichment_jobs : la ligne doit rester drainable,
        // sinon enrich_item — seul chemin de téléchargement — ne s'exécute jamais.
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM enrichment_queue WHERE state IN ('pending','error')"
            ),
            1,
            "le fichier cloud doit rester dans la file jusqu'au téléchargement"
        );
        // Et la couverture le compte comme 100 % enrichi.
        assert_eq!(
            count(
                &db,
                "SELECT COALESCE(SUM(embedding_ready),0) FROM enrichment_queue
                 WHERE state NOT IN ('ineligible','removed')"
            ),
            1
        );
        let body: String = db
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT body FROM items WHERE id='drive-1'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            body, metadata_body,
            "le rattrapage ne télécharge rien lui-même : c'est enrich_item, via la file, qui le fera"
        );

        let _ = std::fs::remove_dir_all(root);
    });
}

/// Constat déterministe du mécanisme derrière l'échec de `tests/e2e.rs` : après
/// un scan catalogue, un document est trouvable par son NOM mais son extrait est
/// VIDE. Une question portant sur le CONTENU ne peut donc pas être répondue tant
/// que l'enrichissement n'est pas passé, et rien dans l'outil `files.search` ne
/// programme cet enrichissement.
#[test]
fn un_document_catalogue_remonte_avec_un_extrait_vide() {
    runtime().block_on(async {
        let root = std::env::temp_dir().join(format!("syn-audit-snippet-{}", uuid::Uuid::new_v4()));
        let docs = root.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("devis_alpha.md"),
            "# Devis projet Alpha\nLe montant total du devis pour le projet Alpha est de 12 400 euros.",
        )
        .unwrap();

        let db = Db::open(&root.join("syn.db"), &"9".repeat(64)).unwrap();
        db.with(|connection| {
            connection.execute(
                "INSERT INTO settings(key,value) VALUES ('embed_model','\"audit\"')
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

        let llm: Arc<dyn LlmClient> = Arc::new(AuditEmbedder);
        let indexer = Indexer::start(db.clone(), llm.clone(), Bus::new(), "audit".into());
        indexer
            .tx
            .send(IndexJob::FullScan(Some(docs.clone())))
            .unwrap();
        for _ in 0..300 {
            let status = indexer.status(&db).unwrap();
            if status.catalog_ready && status.eligible_count >= 1 && !status.running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let results = syn_app::retrieval::search(&db, &llm, "montant du devis projet Alpha", 5)
            .await
            .unwrap();
        let found = results
            .iter()
            .find(|item| item.source_ref.contains("devis_alpha"))
            .expect("le document devrait être trouvable par son nom");
        assert!(
            found.snippet.is_empty(),
            "extrait attendu vide après un scan catalogue : {:?}",
            found.snippet
        );

        indexer.stop_and_wait().await;
        let _ = std::fs::remove_dir_all(root);
    });
}
