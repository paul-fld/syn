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
`llama3.1:latest` (standard, 8B) / `qwen3:14b` (costaud) + `nomic-embed-text` (embeddings).

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
  sous Accès complet au disque) + **envoi/brouillons réels via Apple Mail**
  (AppleScript, toujours derrière le plancher) ; **Messages** (chat.db local, groupé
  par correspondant/mois, rattaché aux personnes) ; **Rappels** (EventKit, miroir
  bidirectionnel avec `tasks`) ; Contacts macOS (best-effort) ; Calendrier EventKit
  (lecture + création, **miroir local pour la proactivité**, invités ⇒ plancher) ;
  Système (sysinfo + pmset, diagnostic explicable) ; Contexte d'écran v0 ; lecture à
  voix haute (`say`). Google Workspace et Microsoft 365 sont opérationnels en PKCE :
  synchronisation Gmail/Outlook, Drive/OneDrive et calendriers, recherche locale
  chiffrée, renouvellement de jeton, envoi de mail et création d'événement confirmés.
  GitHub utilise le Device Flow et Slack un callback HTTPS.
- **Recherche** — normalisation française côté SQLite (`syn_fold` : accents/casse),
  mots vides filtrés, radicaux singulier/pluriel, recherche structurée
  (événements/tâches/engagements/personnes) fusionnée au retrieval, transparence
  d'état d'index dans `files.search` (le modèle sait si l'indexation est en cours).
- **Mémoire longue** — au-delà de 18 tours, les échanges anciens d'une session sont
  condensés en résumé (`sessions.summary`) réinjecté dans le system prompt.

## OAuth de développement

Les inscriptions fournisseur ne doivent pas être publiées pour tester les connexions.
Crée les clients de développement, copie `.env.example` vers `.env`, puis renseigne
les identifiants locaux. Les scripts Tauri chargent automatiquement ce fichier avant
`npm run tauri dev`. Les jetons obtenus sont stockés dans le trousseau du système,
jamais dans Git ni dans la base Syn.

Google et Microsoft n'utilisent aucun secret embarqué. GitHub ne demande que le
Client ID du Device Flow. Slack exige son secret et un callback HTTPS : utilise en
développement un tunnel HTTPS vers le port local configuré.

Microsoft doit enregistrer `http://localhost/oauth/callback` comme URI « Mobile et
bureau » et autoriser les permissions déléguées `User.Read`, `Mail.Read`,
`Mail.Send`, `Calendars.ReadWrite`, `Files.ReadWrite.All` et `Sites.Read.All`.
`Sites.Read.All` est ce qui rend SharePoint et les fichiers partagés visibles à la
recherche ; sans lui, seul le OneDrive personnel répond. Google doit utiliser un
client « Application de bureau », activer Gmail, Calendar et Drive, puis autoriser
`gmail.readonly`, `gmail.send`, `calendar`, `drive.readonly` et `drive.file`.
`drive.file` n'ouvre l'écriture que sur les documents créés par Syn : il permet
`document.create` vers Google Docs sans donner de droit d'écriture sur le reste du
Drive. En mode test, le compte utilisé doit figurer parmi les utilisateurs de test
Google.

Ces portées ont changé : un compte autorisé avant cette version doit être
déconnecté puis reconnecté dans **Connecteurs** pour que la recherche SharePoint et
la création de documents fonctionnent.

Après autorisation dans Connecteurs, le bouton **Synchroniser** construit le miroir
local. Syn l'actualise ensuite toutes les 30 minutes. Déconnecter un compte supprime
ses jetons du trousseau et retire immédiatement son miroir de l'index actif.

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

Synchronisation métier des API Google/Microsoft/Slack/GitHub après authentification
(l'envoi de mail passe par Apple Mail en attendant), vision/CLIP des photos (EXIF et
recherche lexicale opérationnels), dictée STT (la lecture TTS est active), escalade
cloud (toggle présent, egress fermé), thème clair, updater signé/notarisation,
reconnaissance faciale **[V2]**, réunions **[V2]**, module enfant **[V2]**,
coercitif des modes **[V2]**.

## Décision produit — accès disque (14/08/2026)

L'accès complet au disque est **assumé** : une autorisation unique, Syn indexe tout le
répertoire utilisateur (exclusions techniques près) et lit les documents sensibles par
défaut (`sensitive_consent: true`, opt-out dans Réglages ▸ Confidentialité). Le
moindre privilège dossier-par-dossier décrit dans la doc Files historique n'est plus
le comportement du produit.

## Tests

```bash
cd src-tauri && cargo test
```

36 tests : plancher inviolable à tous les niveaux d'autonomie, invités ⇒ plancher,
untrusted ⇒ confirmation, exclusions d'indexation, détection sensible, refus de
dissolution des garde-fous, tolérance au risque propre acceptée, extraction du profil
de voix, classification des règles, chiffrement wrap/unwrap, suite d'injection
(fermeture de bloc, destinataire/URL exfiltrés), normalisation de recherche française
(mots vides, pluriels, accents) et retrouvabilité d'un document réel (« quittances de
loyer » au pluriel → « Quittance de loyer » au singulier), chaîne e2e complète.
