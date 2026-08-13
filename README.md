# Syn

Assistant de vie numérique **desktop, local-first et souverain** — une couche de
mémoire + récupération + action posée sur la vie numérique de l'utilisateur, qu'il
possède entièrement. Construit d'après la documentation technique v3 et les maquettes UI.

## Lancer

```bash
# Prérequis dev : Rust, Node 22+, Ollama (runtime d'inférence de dev)
cd syn
npm install
npm run tauri dev        # dev
npm run tauri build      # bundle .app / .dmg
```

Au premier lancement : onboarding (mot de passe maître → autorisations → services →
mise en route : détection matérielle, téléchargement des modèles et première indexation
automatique après autorisation macOS). Les modèles par palier : `llama3.2:3b` (léger) /
`llama3.1:8b` (standard) / `qwen3:14b` (costaud) + `nomic-embed-text` (embeddings).

## Architecture

- **Tauri v2** — backend **Rust** (daemon, tray, event bus tokio) + frontend **SolidJS**.
- **SQLite + SQLCipher (AES-256)** — schéma normatif §8 dans `migrations/0001_init.sql`.
  Clé = 32 octets aléatoires, enveloppée par le mot de passe maître (Argon2id +
  ChaCha20-Poly1305) et par une phrase de récupération de 12 mots. Trousseau OS opt-in.
  Pas de récupération serveur : sans mot de passe ni phrase, données irrécupérables.
- **`LlmClient`** (`generate`/`embed`) — `OllamaClient` en dev ; le moteur embarqué de
  prod (candle / mistral.rs / llama.cpp) se branchera derrière la même interface.
- **Retrieval hybride** — SQL (mots-clés/métadonnées) + vecteurs (cosinus en Rust,
  brute-force sur la table `embeddings`), fusion + récence, budget de contexte strict,
  assemblage **sourcé** (`[source:N]` → chemin ouvrable).
- **Boucle agentique** (`router/`) — perceive → retrieve → plan → act → observe →
  respond, bornée à 5 itérations d'outils ; la confirmation est un point d'arrêt
  **dans** la boucle.
- **Porte d'action** (`actions/`) — classes de risque (read / réversible-local /
  réversible-externe / **plancher**). Le plancher (irréversible, personne réelle,
  financier/administratif) n'est **jamais** désactivable. Preview + undo pour le
  réversible. Tool-call dérivé de contenu non fiable ⇒ confirmation forcée (§3.4).
- **Sécurité de l'agent** — séparation instructions/données (marqueurs `<<<DONNÉES…>>>`
  neutralisés contre la fermeture prématurée), contrôle d'egress (loopback seul par
  défaut), journal d'accès, suite de tests d'injection (`cargo test`).
- **Connecteur Files** — autorisation unique via Accès complet au disque, puis walk + exclusions strictes (fix Minecraft : caches,
  `node_modules`, bundles, jeux, dossiers cachés), projets de code = unité atomique,
  gate de lecture des sensibles (métadonnées seules sans consentement), incrémental
  (blake3 + mtime), watching débouncé 2 s, throttlé, skip+log jamais de crash.
- **Rangement intelligent** (`tools/reorganize.rs`) — scan → classification
  multi-signaux (déterministe + LLM par lot) → plan avec confiances → revue unique →
  exécution + **undo global** ; quarantaine « Éléments à supprimer », jamais de
  suppression.
- **Règles** (`rules/`) — genres style / standing / action_modifier classés à l'ajout ;
  refus des règles anti-sécurité (garde déterministe + LLM) ; conflits arbitrés par
  l'utilisateur ; **profil de voix** structuré → catalogue de libellés templatés
  tu/vous (`src/lib/voice.ts`).
- **Proactivité** (`proactivity/`) — arbitre unique : budget de rareté, fenêtres
  calmes, anti-répétition, `urgent` passe toujours ; brief de démarrage (gate
  jour/activité/heure-plancher), débrief, gardien système, engagements, événements
  imminents. Chaque surfaçage porte sa raison.
- **Connecteurs V1** — Files (complet) ; Apple Mail natif local (.emlx, lecture seule,
  sous Accès complet au disque) ; Contacts macOS (best-effort) ; Calendrier local
  (events, invités ⇒ plancher) ; Système (sysinfo + pmset, diagnostic explicable) ;
  Contexte d'écran v0 (app/fenêtre au premier plan). OAuth de développement prêt :
  Google/Microsoft en PKCE, GitHub en Device Flow et Slack via callback HTTPS.

## OAuth de développement

Les inscriptions fournisseur ne doivent pas être publiées pour tester les connexions.
Crée les clients de développement, puis exporte les variables décrites dans
`.env.example` avant `npm run tauri dev`. Les jetons obtenus sont stockés dans le
trousseau du système, jamais dans Git ni dans la base Syn.

Google et Microsoft n'utilisent aucun secret embarqué. GitHub ne demande que le
Client ID du Device Flow. Slack exige son secret et un callback HTTPS : utilise en
développement un tunnel HTTPS vers le port local configuré.

## Décisions 🔎 tranchées à ce build

| Question | Décision |
|---|---|
| Modèles par palier | llama3.2:3b / llama3.1:8b / qwen3:14b + nomic-embed-text (licences permissives) |
| Runtime | Ollama (dev) ; interface `LlmClient` prête pour le moteur embarqué |
| Index vectoriel | BLOB f32 + cosinus Rust (suffisant à l'échelle locale ; extension SQLite possible plus tard) |
| Extraction | pdf-extract (confiné anti-panic), zip+quick-xml (docx/pptx), calamine (xlsx), kamadak-exif (photos) |
| Watching FS | notify + debouncer 2 s |
| Raccourci barre | **⌥ Espace** (pas de conflit Spotlight) |
| Récupération de clé | enveloppe double (mot de passe / phrase 12 mots) + trousseau OS opt-in |

## Hors de ce build (honnêtement statué dans l'UI)

Synchronisation métier des API Google/Microsoft/Slack/GitHub après authentification,
envoi SMTP/OAuth réel (l'ossature plancher/brouillon est complète), vision/CLIP des
photos (EXIF opérationnel), voix STT/TTS (toggles présents),
escalade cloud (toggle présent, egress fermé), reconnaissance faciale **[V2]**,
réunions **[V2]**, module enfant **[V2]**, coercitif des modes **[V2]**.

## Tests

```bash
cd src-tauri && cargo test
```

25 tests : plancher inviolable à tous les niveaux d'autonomie, invités ⇒ plancher,
untrusted ⇒ confirmation, exclusions d'indexation, détection sensible, refus de
dissolution des garde-fous, tolérance au risque propre acceptée, extraction du profil
de voix, classification des règles, chiffrement wrap/unwrap, suite d'injection
(fermeture de bloc, destinataire/URL exfiltrés).
