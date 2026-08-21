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
- **Réflexes** (`proactivity/reflexes.rs`) — ce que Syn remarque de la VIE de
  l'utilisateur, et non de sa machine : messages restés sans réponse (d'un
  correspondant habituel, adressés à lui, sans envoi depuis), réunion imminente
  **avec les derniers échanges** des participants, engagement pris et sans suite,
  dossier qui déborde, anniversaire à venir. Tous déterministes (aucun appel au
  modèle), inscrits dans `triggers` donc **visibles et débrayables** un par un dans
  « Mes programmations », chacun avec son propre rythme d'évaluation. Un réflexe qui
  ne sait pas se tait : sans adresse connue de l'utilisateur, le suivi des messages
  ne devine rien.
- **La toile** (`memory/graph.rs`) — graphe personnel typé, dérivé des sources déjà
  indexées : `ecrit_a`, `auteur_de`, `apparait_dans`, `co_destinataire`, `reunit`,
  `classe_dans`. Chaque arête porte sa date, sa provenance et son nombre
  d'observations — donc vérifiable, corrigeable, effaçable (l'inverse d'un réseau de
  neurones). Construction incrémentale par curseur, en une transaction par passe,
  sous le même budget que l'indexation. Les correspondants vivent dans `contacts`,
  pas dans le carnet : `people` reste ce que l'utilisateur a choisi d'y mettre.
  Les adresses de l'utilisateur sont déduites par PRÉSENCE (il figure dans presque
  tous ses messages) ; **sans majorité nette, Syn ne tranche pas et demande**.
- **Ligne de temps** (`memory/timeline.rs`) — chronologie unifiée mails reçus /
  envoyés, documents, rendez-vous, engagements, actions de Syn, conversations.
  Aucune donnée dupliquée : les tables existantes sont lues par leur date, chaque
  source bornée séparément pour qu'une boîte bavarde n'éclipse pas le reste.
- **Habitudes** (`memory/habits.rs`) — mémoire procédurale : compte d'envoi réel,
  formules d'ouverture/clôture, heures de travail, dossiers de rangement, déduits de
  `actions_log` et des mails envoyés. Une habitude est **observée** (≥ 3 fois), puis
  **proposée**, et n'entre dans le system prompt qu'une fois **confirmée** par
  l'utilisateur. Un rejet tient : réobserver ne la ressuscite pas.
- **Trois mémoires, trois outils** — `memory.query` (où est cette chose ?),
  `memory.timeline` (que s'est-il passé ?), `memory.relations` (qui est relié à
  quoi ?). Tout est inspectable dans Connaissances ▸ Ta toile / Chronologie /
  Habitudes, y compris la reconstruction complète de la toile.
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
- **Langues** — Syn répond dans la langue de son utilisateur (`i18n`), détectée sur
  ses phrases par des mots grammaticaux (classes fermées, jamais le vocabulaire du
  sujet : « retrouve le mail de Liverpool » reste du français), mémorisée par
  conversation pour qu'un « ok » ne fasse basculer personne, et surchargeable dans
  Réglages ▸ Personnalisation. Français et anglais sont couverts de bout en bout :
  réponses du modèle, phrases écrites par Syn lui-même (recherches, briefs,
  réflexes, parcours d'envoi), et langue de rédaction des mails. La langue de
  TRAVAIL interne — consignes de raisonnement, mots envoyés aux fournisseurs — est
  l'anglais, que les modèles suivent le mieux et que parlent les services.
  *Reste en français* : le chrome de l'interface (menus, titres, libellés des
  réglages).
- **Mots de recherche** (`router/search_terms.rs`) — le modèle traduit la demande en
  mots-clés, dans sa langue ET en anglais : un message rédigé en anglais ne contient
  ni « décembre » ni « tickets ». Appel SÉPARÉ de la classification d'intention
  (y ajouter des consignes avait été mesuré à 4,3 % → 17,4 % d'erreur) et
  facultatif : hors ligne, l'extraction déterministe reprend la main. Les mots
  proposés n'ont aucune faveur — ils passent par la même mesure de rareté, donc un
  mot inventé est simplement « inconnu » et ne mène jamais la recherche. Mesuré par
  `mots_de_recherche_eval` sur des demandes multilingues et multi-domaines.
- **Recherche de messages** — les mots de la demande sont classés par leur rareté
  MESURÉE dans la messagerie de l'utilisateur (`retrieval::ranked_terms`) : « mail »
  ou « décembre » y sont partout, « liverpool » dans une poignée de messages. Les
  requêtes au fournisseur retirent ensuite le mot le plus banal, l'un après
  l'autre, jusqu'au seul mot porteur — **jamais** une disjonction (« au moins un de
  ces mots »), qui rendait la boîte entière comme si c'était une réponse. Les
  résultats sont reclassés par Syn (objet > corps, dates reconnues dans les deux
  langues via `date_variants`), ceux qui ne correspondent à rien sont écartés dès
  qu'il existe mieux, et un message qui domine nettement est donné comme réponse
  plutôt que noyé dans une liste.
- **Rangement de boîte mail** — « range Gmail » et « range Outlook » lancent un
  audit strictement limité au compte nommé. Les compteurs et cohortes viennent
  directement du fournisseur ; l'interface distingue les messages recensés des
  candidats réellement inspectés, des cas ambigus et des messages laissés en
  place. Syn protège d'abord les contenus durables, regroupe les campagnes,
  prépare un plan borné, détecte les désabonnements RFC 8058 « one click », puis
  attend une confirmation unique. Le plan complet est figé dans la base locale
  chiffrée et l'exécution produit un journal d'annulation ; les désabonnements,
  définitifs, sont signalés séparément avant validation.
- **Règles de tri prioritaires** — une règle de Réglages comme « archive mes mails
  de factures Anthropic » devient une contrainte structurée (action, type,
  expéditeur et éventuellement fournisseur), appliquée avant les heuristiques
  lors du rangement. Deux règles contradictoires demandent un arbitrage.
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
`Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`, `Files.ReadWrite.All` et `Sites.Read.All`.
`Sites.Read.All` est ce qui rend SharePoint et les fichiers partagés visibles à la
recherche ; sans lui, seul le OneDrive personnel répond. Google doit utiliser un
client « Application de bureau », activer Gmail, Calendar et Drive, puis autoriser
`gmail.readonly`, `gmail.modify`, `gmail.send`, `calendar`, `drive.readonly` et `drive.file`.
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
