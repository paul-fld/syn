//! Test de bout en bout (Phase 0-1) : clé → base chiffrée → indexation d'un
//! dossier → retrieval hybride → boucle agentique → réponse sourcée.
//! Nécessite Ollama (runtime de dev) ; sinon, valide le mode dégradé.

use std::sync::Arc;
use syn_app::bus::Bus;
use syn_app::connectors::files::{IndexJob, Indexer};
use syn_app::db::Db;
use syn_app::llm::ollama::OllamaClient;
use syn_app::llm::LlmClient;
use syn_app::security::egress::EgressGuard;
use syn_app::security::keys::KeyStore;
use syn_app::state::Core;

#[test]
fn chaine_complete_locale() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    tauri::async_runtime::set(rt.handle().clone());
    rt.block_on(async {
        let tmp = std::env::temp_dir().join(format!("syn-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 1. Clé maîtresse + phrase de récupération.
        let ks = KeyStore::new(&tmp);
        let (key, phrase) = ks.setup(Some("test@syn.local".into()), "motdepasse-test").unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);
        assert_eq!(ks.unlock_password("motdepasse-test").unwrap(), key);
        assert_eq!(ks.unlock_phrase(&phrase).unwrap(), key);
        assert!(ks.unlock_password("mauvais-mdp").is_err());

        // 2. Base chiffrée (SQLCipher) + migrations.
        let db = Db::open(&tmp.join("syn.db"), &key).unwrap();
        // Une mauvaise clé ne doit pas ouvrir la base.
        assert!(Db::open(&tmp.join("syn.db"), &"0".repeat(64)).is_err());

        // 3. Dossier de test avec du contenu réel + un piège d'exclusion.
        let docs = tmp.join("docs");
        std::fs::create_dir_all(docs.join("node_modules")).unwrap();
        std::fs::write(
            docs.join("devis_alpha.md"),
            "# Devis projet Alpha\nLe montant total du devis pour le projet Alpha est de 12 400 euros, \
             valable jusqu'au 30 septembre 2026. Contact : Marie Dupont.",
        )
        .unwrap();
        std::fs::write(docs.join("notes_reunion.txt"), "Réunion de mardi : le lancement de la fusée est reporté à novembre.").unwrap();
        std::fs::write(docs.join("node_modules").join("piege.md"), "Ceci ne doit JAMAIS être indexé (fix Minecraft).").unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO folders (path, added_at, status) VALUES (?1, 0, 'active')",
                [docs.to_string_lossy().to_string()],
            )?;
            Ok(())
        })
        .unwrap();

        // 4. LlmClient (Ollama dev) derrière le contrôle d'egress.
        let egress = Arc::new(EgressGuard::new());
        assert!(egress.check("https://evil.example.com/exfil").is_err(), "egress ouvert !");
        let llm: Arc<dyn LlmClient> =
            Arc::new(OllamaClient::new("http://127.0.0.1:11434", "llama3.1:latest", "nomic-embed-text", egress));
        let ollama_up = llm.status().await.available;

        // 5. Indexation (walk + exclusions + chunk + embed).
        let bus = Bus::new();
        let indexer = Indexer::start(db.clone(), llm.clone(), bus.clone(), "nomic-embed-text".into());
        indexer.tx.send(IndexJob::FullScan(None)).unwrap();
        for _ in 0..120 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let st = indexer.status(&db).unwrap();
            if !st.running && st.items_count >= 2 {
                break;
            }
        }
        let st = indexer.status(&db).unwrap();
        assert!(st.items_count >= 2, "indexation incomplète : {} items", st.items_count);
        let piege: i64 = db
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM items WHERE source_ref LIKE '%piege%'", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(piege, 0, "l'exclusion node_modules a échoué");

        // 6. Retrieval hybride, sourcé.
        let results = syn_app::retrieval::search(&db, &llm, "montant du devis projet Alpha", 5).await.unwrap();
        assert!(!results.is_empty(), "retrieval vide");
        assert!(
            results[0].source_ref.contains("devis_alpha"),
            "mauvais premier résultat : {}",
            results[0].source_ref
        );

        // 7. Boucle agentique complète (si le runtime est disponible).
        let core = Core {
            db: db.clone(),
            llm: llm.clone(),
            bus: bus.clone(),
            indexer,
            key_hex: Arc::new(std::sync::Mutex::new(key.clone())),
        };
        let answer = syn_app::router::handle_query(&core, "session-test", "Quel est le montant du devis du projet Alpha ?")
            .await
            .unwrap();
        println!("RÉPONSE : {} (dégradé: {})", answer.text, answer.degraded);
        if ollama_up {
            assert!(
                answer.text.contains("12 400") || answer.text.contains("12400") || !answer.sources.is_empty(),
                "réponse non ancrée : {}",
                answer.text
            );
        } else {
            assert!(answer.degraded, "sans runtime, le mode dégradé doit être signalé");
        }

        // 7b. Les projets regroupent réellement la mémoire des conversations.
        db.with(|c| {
            c.execute(
                "INSERT INTO projects (id, name, created_at, updated_at) VALUES ('project-test','Alpha',0,0)",
                [],
            )?;
            c.execute(
                "UPDATE sessions SET project_id='project-test' WHERE id='session-test'",
                [],
            )?;
            c.execute(
                "INSERT INTO sessions (id,title,created_at,updated_at,project_id)
                 VALUES ('session-soeur','Décisions',0,0,'project-test')",
                [],
            )?;
            c.execute(
                "INSERT INTO conversations (session_id,turn,role,content,created_at)
                 VALUES ('session-soeur',0,'user','La couleur retenue est le vert.',0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let project_context = syn_app::memory::project_context(&db, "session-test", 10)
            .unwrap()
            .expect("mémoire de projet absente");
        assert_eq!(project_context.1, "Alpha");
        assert!(project_context.2.contains("couleur retenue est le vert"));

        // 8. Porte d'action : un envoi de mail doit rester en attente (plancher).
        let pending_before = syn_app::actions::list_pending(&db).unwrap().len();
        let id = syn_app::actions::queue_pending(
            &db,
            "mail.send",
            &serde_json::json!({"to": "x@y.fr", "subject": "test", "body": "…"}),
            syn_app::actions::RiskClass::Floor,
            "Envoyer un mail à x@y.fr",
            false,
            Some("session-test"),
        )
        .unwrap();
        assert_eq!(syn_app::actions::list_pending(&db).unwrap().len(), pending_before + 1);
        syn_app::actions::set_action_result(&db, &id, "rejected", None, None).unwrap();

        // 9. Un verrouillage doit arrêter le watcher et la boucle d'indexation.
    core.indexer.stop_and_wait().await;
        assert!(
            core.indexer.stopped.load(std::sync::atomic::Ordering::SeqCst),
            "l'indexeur conserve la base ouverte après l'arrêt"
        );

        std::fs::remove_dir_all(&tmp).ok();
    });
}
