# Audit complet de Syn — 14 août 2026

## Mise à jour après correction

Les constats ci-dessous décrivent l'état audité initial. Une première vague de correction a depuis été implémentée dans le dépôt :

- recherche de fichiers routée de façon déterministe, sans génération libre du LLM ;
- exclusion obligatoire des résultats sémantiques sans preuve lexicale, scoring centré sur la couverture et abstention explicite ;
- conservation de l'intention après une relance telle que « ces fichiers n'ont rien à voir » ;
- index plein texte FTS5 local/chiffré, filtrage des embeddings par modèle et OCR local de secours pour PDF scannés ainsi que les images de type document ;
- réconciliation des fichiers disparus, suppression des anciens embeddings lorsque le contenu devient illisible et visibilité du nombre de fichiers non extractibles ;
- indexation des sources utiles à l'intérieur des projets de code et calcul de leur activité récente ;
- demande déterministe du contenu d'un mail absent, blocage des adresses inventées et confirmation naturelle d'un envoi en attente ;
- arrêt de la création automatique d'engagements depuis les mails reçus, pagination progressive de Mail, correction de Rappels et réconciliation du miroir Calendrier ;
- copie temporaire Messages aléatoire, permissions `0700/0600` et nettoyage RAII ;
- permissions Tauri séparées : commandes complètes pour `main`, sous-ensemble minimal pour `bar`, et redirections HTTP désactivées vers Ollama ;
- états honnêtes pour les OAuth sans connecteur métier, rendu sûr des listes/gras/code, focus clavier visible et dialogues mieux exposés aux technologies d'assistance ;
- Vite mis à niveau vers 8.2.1 (audit npm à zéro vulnérabilité au moment de la correction) et workflow CI ajouté.

Validation après cette vague : 43 tests unitaires Rust et 1 test d'intégration, Clippy avec `-D warnings`, TypeScript, build Vite et `git diff --check` réussis. Les travaux structurels non encore clos restent explicitement décrits dans les sections correspondantes : connecteurs cloud métier, pagination historique complète de Messages, véritable banc d'évaluation à grande échelle, pilotage généralisé des applications et formats supplémentaires.

## Verdict exécutif

Syn n'est pas « n'importe quoi » : le dépôt contient une base technique sérieuse — application Tauri/Rust, base SQLCipher, récupération hybride, journal d'actions, confirmations déterministes et interface visuellement cohérente. Mais ce socle est encore un **prototype avancé**, pas encore l'assistant desktop transversal promis.

Le problème central n'est pas le niveau d'accès au disque. L'accès global au répertoire utilisateur est une décision produit cohérente avec la promesse de Syn et cet audit ne recommande pas de revenir à une sélection dossier par dossier. Le vrai problème est l'écart entre :

- les données que Syn est théoriquement autorisé à voir ;
- les données qu'il indexe réellement et correctement ;
- les outils qu'il sait réellement appeler ;
- la fiabilité avec laquelle le petit modèle local choisit ces outils ;
- ce que l'interface laisse croire.

Aujourd'hui, Syn peut retrouver certains fichiers textuels indexés, rechercher des mails Apple locaux, créer/envoyer un mail simple après confirmation, lire/créer des événements simples, gérer des tâches locales, connaître quelques personnes, diagnostiquer l'appareil et proposer quelques alertes. Il ne peut pas encore observer continuellement le travail réel, comprendre l'écran visuellement, piloter librement les applications, exploiter les services cloud affichés, retrouver de façon fiable n'importe quel document, ni conduire des workflows multi-applications robustes.

**Appréciation de préparation produit : 4/10.** Le socle est prometteur, mais plusieurs défauts P0/P1 rendent une diffusion publique prématurée. Les deux échecs montrés — adresse mail inventée et quittance remplacée par le contexte d'un projet — sont représentatifs d'un manque de tests de comportement et d'un routage trop confié au modèle. Des protections ont depuis été ajoutées dans le code pour ces deux scénarios précis, mais il faut généraliser la démarche à toutes les intentions.

## Périmètre et méthode

Audit statique et dynamique de :

- environ 12 700 lignes de Rust et 6 000 lignes TypeScript/TSX/CSS ;
- architecture, migrations, indexation, retrieval, mémoire, routeur, outils et actions ;
- Files, Mail, Messages, Contacts, Calendrier, Rappels, écran, système et OAuth ;
- proactivité, briefs, programmations, règles et modes ;
- onboarding, navigation, conversations, connaissances, réglages et barre flottante ;
- documentation technique v3 et écarts avec l'implémentation ;
- rendu des écrans de démonstration, y compris les captures fournies.

Validations exécutées :

| Validation | Résultat |
|---|---|
| `cargo test --all-targets --no-fail-fast` | 39 tests unitaires + 1 test d'intégration réussis |
| `npx tsc --noEmit` | réussi |
| `npm run build` | réussi |
| `npm ls --all` | réussi |
| `git diff --check` | réussi |
| `cargo clippy --all-targets --all-features -- -D warnings` | échec : 6 avertissements traités comme erreurs |
| `npm audit` | 2 alertes : 1 haute sur Vite, 1 modérée sur esbuild, dépendances de développement |
| Audit RustSec | non exécuté : `cargo-audit` n'est pas installé |
| Tests frontend / accessibilité / E2E UI | inexistants |
| CI automatisée | aucune workflow détectée |

Limites : je n'ai pas ouvert ni analysé le contenu privé réel de la base Syn de l'utilisateur. La qualité sur un corpus complet du Mac et les performances longue durée doivent être mesurées avec un corpus de test représentatif et anonymisé.

## Priorités absolues

### P0 — à corriger avant toute promesse de fiabilité

#### SYN-P0-01 — Rappels est annoncé opérationnel mais ne peut jamais devenir disponible

`reminders::available()` attend le statut `authorized`, tandis que le pont natif renvoie `granted`. Toute synchronisation, création et complétion de rappel natif est donc court-circuitée. L'UI déclare pourtant Rappels opérationnel et synchronisé avec les tâches.

Impact : fonctionnalité entièrement cassée et promesse trompeuse.

Correctif : unifier les enums de permission en un type Rust fermé, ajouter un test du pont macOS et un test d'intégration création → miroir → complétion → undo.

#### SYN-P0-02 — Du contenu de mail non fiable crée automatiquement des engagements

Chaque mail ingéré passe dans `extract_entities`, qui transforme des chaînes comme « je t'envoie », « à faire : » ou `TODO:` en engagement `owed_by_me`, sans demande utilisateur. Cela contredit explicitement le prompt et la documentation de sécurité, qui disent que les impératifs/TODO contenus dans des données ne deviennent jamais des actions.

En outre, un mail reçu disant « je t'envoie le document » décrit vraisemblablement l'engagement de l'expéditeur, pas celui de l'utilisateur. La direction est donc souvent inversée.

Impact : empoisonnement de la mémoire, fausses alertes, fausses obligations et vecteur d'injection indirecte.

Correctif : ne créer que des **candidats d'engagement** avec provenance, locuteur, confiance et état `proposed`; demander validation ou utiliser un classifieur déterministe + modèle borné avant promotion. Aucun contenu ingéré ne doit écrire directement dans l'état normatif.

#### SYN-P0-03 — La recherche n'est pas encore démontrée fiable à l'échelle de « tout le Mac »

Le correctif récent isole maintenant `files.search` du contexte du projet pour une requête reconnue comme recherche de fichier, ce qui vise précisément l'échec « quittance → Aberration ». Un test vérifie aussi singulier/pluriel. Mais il n'existe aucun banc d'évaluation réaliste : paraphrases françaises, fautes, noms oubliés, documents scannés, homonymes, corpus de plusieurs centaines de milliers de fragments, index partiel et requêtes multi-critères.

La détection de l'intention « recherche de fichier » reste heuristique. Une formulation non reconnue retombe sur la récupération générale et peut réinjecter le projet attaché. Le modèle doit ensuite appeler le bon outil malgré ce contexte.

Impact : la promesse cœur — « retrouve-moi ce document dont j'ai oublié le nom et l'emplacement » — n'est pas prouvée.

Correctif : routeur d'intentions déterministe/structuré avant le LLM, outil de recherche toujours appelé pour les verbes de recherche, suite d'évaluation avec au moins 300 requêtes réelles, métriques Recall@5/MRR/taux de mauvais domaine, réécriture de requête, reranker et UI de résultats plutôt qu'une réponse narrative unique.

#### SYN-P0-04 — Les connecteurs cloud sont des authentifications sans fonctionnalités

Google, Microsoft, Slack et GitHub peuvent obtenir et stocker un jeton, mais aucune API métier ne synchronise Gmail, Drive, Outlook, OneDrive, Slack ou GitHub. Le statut devient pourtant `connected` et l'interface décrit les services correspondants.

Impact : « connecté » signifie seulement « jeton présent », pas « données disponibles ». C'est un défaut de vérité produit.

Correctif : états distincts `configured`, `authorizing`, `authorized`, `syncing`, `ready`, `degraded`, `expired`; afficher les capacités effectives par service. Tant que l'adaptateur métier n'existe pas, libeller « Authentification de développement — aucune donnée synchronisée » et ne pas l'exposer dans l'onboarding public.

#### SYN-P0-05 — Synchronisations incomplètes et données fantômes

- Mail ne considère que les 800 `.emlx` les plus récents à chaque passe. Une fois ceux-ci connus, la passe les ignore mais ne pagine pas vers les suivants : l'historique plus ancien peut ne jamais être indexé.
- Mail ignore tout message déjà connu sans recalculer son hash : les modifications ne sont pas vues.
- Messages ne lit que les 4 000 derniers messages globaux, sans pagination ; les contacts très actifs évinceraient les autres.
- Calendrier synchronise seulement J-1 à J+30 et ne retire pas les événements supprimés/annulés.
- Un scan Files ne réconcilie pas les fichiers supprimés pendant que Syn était arrêté.
- La suppression d'un dossier peut ne marquer que le dossier, laissant ses descendants actifs dans l'index.
- Si une extraction devenue impossible produit `content=None`, les anciens embeddings ne sont pas effacés.

Impact : réponses fausses, anciennes ou incomplètes sans avertissement visible.

Correctif : curseurs de pagination persistés, journal de synchronisation, réconciliation par source, tombstones, métriques de couverture, « dernière synchro complète », et invalidation transactionnelle des embeddings.

#### SYN-P0-06 — L'indexation de projet empêche le niveau de contexte attendu

Un projet de code est traité comme un seul bloc contenant le README (8 000 caractères) et jusqu'à 200 noms de fichiers sur trois niveaux. Le code source n'est pas indexé. Le hash du projet dépend seulement de ce résumé ; une modification de code sans changement d'arborescence/README peut donc rester invisible. Le mtime du dossier n'est pas un journal fiable de l'activité dans ses fichiers.

Impact : Syn ne sait pas réellement ce que l'utilisateur faisait dans le projet hier et ne peut pas aider sérieusement sur le code, malgré la suggestion « continuer ce projet ».

Correctif : conserver le projet comme unité de déplacement, mais indexer ses fichiers comme enfants recherchables ; construire une entité projet, un graphe fichiers/git/éditeur et une timeline d'activité.

#### SYN-P0-07 — Les commandes Tauri métier sont exposées à toutes les fenêtres

La même capability couvre `main` et `bar`. Selon la documentation officielle Tauri, les commandes enregistrées sont par défaut utilisables par toutes les fenêtres/webviews si elles ne sont pas explicitement déclarées et bornées. La barre obtient donc potentiellement les mêmes commandes métier que la fenêtre principale, y compris des commandes de données sensibles.

Impact : rayon d'impact inutilement large en cas de compromission du frontend de la barre.

Correctif : déclarer les commandes applicatives dans le manifeste Tauri, créer des permissions distinctes, donner à `bar` uniquement requête/capture/affichage, réserver sécurité, purge, réglages, connexions et mutations à `main`.

#### SYN-P0-08 — Aucune évaluation comportementale du modèle et des outils

Les 40 tests actuels couvrent surtout des fonctions déterministes. Ils ne mesurent pas le choix d'outil, les hallucinations d'adresse, les conversations multi-tours, la reformulation, les refus, les résultats vides, les erreurs de connecteur ni la qualité de réponse sur les trois modèles. Aucun seuil qualité ne bloque une release.

Impact : les scénarios qui définissent Syn peuvent casser alors que tous les tests passent.

Correctif : harnais d'evals avec LLM réel ou sorties enregistrées, corpus français versionné, assertions sur appels d'outils et arguments, jeux adversariaux, matrices par modèle/machine, CI et critères de release.

### P1 — défauts majeurs

#### Intelligence et récupération

1. La recherche vectorielle charge tous les embeddings et calcule le cosinus en Rust à chaque requête. Ce brute-force ne tient pas la promesse « tout le disque » à grande échelle.
2. Les embeddings ne sont pas filtrés par modèle courant. Après changement de modèle, anciens vecteurs et nouvelles requêtes peuvent avoir des dimensions incompatibles et cohabiter silencieusement.
3. La fusion de score est artisanale : seuil sémantique fixe `0.35`, bonus de récence pouvant favoriser un document récent peu pertinent, recherche mot-clé en `OR`, pas de reranker ni de calibration.
4. L'analyse française est rudimentaire : suppression naïve de certains `s/x`, pas de lemmatisation, pas de correction orthographique, pas d'acronymes métier ni synonymes appris.
5. La fenêtre de conversation est limitée aux 12 derniers tours ; le résumé long est régénéré à partir d'une fenêtre ancienne qui peut réinclure plusieurs fois les mêmes tours, créant dérive et duplication.
6. Le résumé de conversation est produit par le modèle puis réinjecté comme « déjà validé », alors qu'il peut contenir omissions ou hallucinations.
7. La boucle est limitée à cinq itérations et 1 200 tokens sans stratégie de plan/reprise ; suffisant pour une tâche courte, fragile pour un workflow multi-service.
8. Les observations d'outils ne sont pas persistées avec la conversation. L'audit, la reprise et le débogage exact d'une décision sont incomplets.
9. Les citations n'apparaissent que si le modèle recopie correctement `[source:N]`; les entités calendrier/tâches/personnes n'ont pas toujours une cible ouvrable.
10. `files_search` côté IPC utilise encore la recherche générale puis filtre, ce qui peut reproduire l'éviction avant `LIMIT` en dehors de l'outil agent corrigé.

#### Indexation et formats

1. « Tout le disque » signifie en pratique la racine `home`, pas les volumes externes, partages réseau ou autres emplacements montés. Il faut le dire clairement.
2. Profondeur maximale 12, liens symboliques ignorés, dossiers cachés ignorés et exclusions par simple nom (`Library`, `build`, `dist`, `out`, etc.). Ces règles peuvent masquer des documents personnels légitimes.
3. Limite fichier 200 Mo, lecture texte limitée à 4 Mo, extraction à 400 000 caractères puis seulement 64 chunks d'environ 1 200 caractères : une grande partie des longs documents n'est jamais indexée sémantiquement.
4. Absence d'OCR pour PDF scannés et images : un grand nombre de quittances, factures, courriers et scans sont introuvables par contenu.
5. Absence de Pages, Numbers, Keynote, ODT/ODP, archives, notes Apple, pièces jointes de mail et transcription audio/vidéo.
6. Photos n'utilise pas PhotoKit et ne comprend pas les scènes ; elle ne recherche que les fichiers image indexés avec leurs rares métadonnées EXIF.
7. Les ZIP OOXML sont lus sans plafond explicite de taille décompressée. Un document local malveillant ou corrompu peut provoquer une forte consommation mémoire (zip bomb).
8. Le scan complet collecte d'abord tout en mémoire au lieu de streamer ; le canal de jobs est non borné et ne coalesce pas clairement les tempêtes de fichiers.
9. Le hash lit intégralement jusqu'à 200 Mo de façon synchrone dans une fonction async, ce qui peut bloquer le runtime.
10. Il manque une vue de couverture : fichiers vus, indexés, ignorés, en erreur, tronqués, sans embedding, raison et dernier passage.

#### Mail, calendrier, tâches et personnes

1. Envoi Mail limité à un destinataire, sans CC/BCC, pièce jointe, choix du compte, réponse à un fil, signature ni aperçu riche.
2. Un brouillon Apple Mail peut être créé, mais son undo supprime seulement l'item Syn, pas le vrai brouillon. L'UI peut donc annoncer une annulation incomplète.
3. Compléter une tâche liée à Rappels puis annuler ne rouvre que la tâche locale ; le rappel natif reste complété et la prochaine synchro peut refermer la tâche.
4. Les actions en attente n'ont ni expiration, ni édition structurée, ni invalidation après changement de destinataire/contexte.
5. Le parseur HTML de Mail itère sur les octets et les convertit en caractères ; les corps HTML UTF-8 non ASCII peuvent être corrompus.
6. Mail n'indexe ni pièces jointes, ni états lu/non lu, ni drapeaux, ni dossiers, ni conversations, ni réponses ; le brief appelle tout mail récent « à traiter » sans le savoir.
7. Seul l'expéditeur est utilisé pour apprendre une personne ; les destinataires sortants et alias ne consolident pas correctement le graphe social.
8. Messages copie `chat.db`, WAL et SHM dans un répertoire temporaire prévisible fondé sur le PID, sans permissions/RAII explicites. En cas de crash, une copie sensible peut rester en clair.
9. La copie Messages n'est pas un snapshot atomique ; elle ignore groupes, pièces jointes, réactions, services et certains contenus modernes (`attributedBody`).
10. Les items Messages n'ont pas de date/mtime, ce qui dégrade fortement la récence.
11. Aucun outil agent `messages.search` ou `messages.send` n'existe.
12. Calendrier ne gère pas récurrence, événements journée entière, fuseau explicite, choix d'agenda, notes, mise à jour ou suppression via l'agent.
13. L'erreur « connecte Google ou Microsoft pour inviter des participants » mène vers des connecteurs sans implémentation métier.
14. Contacts est un import ponctuel/best-effort, sans vraie synchronisation, fusion de doublons, modification ou suppression.

#### Sécurité, confidentialité et résilience

1. « Local » réduit fortement l'exposition mais n'annule pas le risque. Avec l'accès global, une compromission de Syn, du WebView, d'une dépendance, d'Ollama ou d'un script AppleScript a un rayon d'accès élevé. C'est acceptable comme choix produit seulement avec durcissement, signature, isolation et audit renforcés.
2. Le contrôle d'egress est appliqué au client Ollama, pas aux requêtes OAuth. Sa documentation « à appeler avant toute requête réseau » n'est donc pas respectée.
3. `reqwest` suit les redirections par défaut : une URL loopback autorisée peut potentiellement rediriger vers un hôte externe sans second contrôle. Désactiver les redirections ou revalider chaque saut.
4. Le prompt dit « rien ne sort », alors que les flux OAuth sortent et que le produit prévoit des connecteurs. Formulation absolue fausse : dire « aucun contenu n'est envoyé sauf au service explicitement connecté pour l'action demandée ».
5. Les tokens OAuth n'ont pas de rafraîchissement, gestion d'expiration ou révocation distante. « Déconnecter » efface seulement le jeton local.
6. Les scopes sont larges dès le départ (`repo`, `Mail.Send`, `Files.Read.All`) alors qu'aucune capacité métier ne les utilise encore. Appliquer scopes progressifs et minimaux.
7. Un secret Slack ne peut pas être considéré secret dans un client desktop distribué. Revoir le flux ou passer par un backend maîtrisé.
8. Les erreurs des tâches OAuth détachées sont silencieuses ; l'UI peut rester dans un état indéterminé.
9. Le verrouillage arrête l'indexeur mais ne possède pas de jeton d'annulation pour les synchronisations déjà lancées avec un clone de la base/clé. Elles peuvent continuer brièvement après « Déconnexion ».
10. Pas de verrouillage automatique sur veille, fermeture de session ou inactivité ; Syn peut rester déverrouillé dans le tray.
11. Pas de limitation du nombre d'essais de mot de passe, délai progressif ni intégration Touch ID. Argon2 protège l'attaque hors ligne, mais l'UX d'authentification peut être renforcée.
12. La purge efface la base et le trousseau maître, mais pas explicitement les jetons OAuth, modèles Ollama, éventuels temporaires abandonnés, préférences locales ou notifications système.
13. « Exporter » ouvre seulement le dossier contenant une base chiffrée et son metadata ; il n'existe ni export portable lisible, ni import/restauration vérifiée.
14. Les notifications système peuvent exposer sujet de mail, événement ou engagement sur un écran verrouillé. Ajouter des niveaux de confidentialité.
15. Pas de signature/notarisation, updater signé, SBOM, audit de licences, audit RustSec, stratégie de divulgation ou builds reproductibles.
16. `npm audit` signale Vite/esbuild de développement. Les avis concernent surtout le serveur de dev, mais doivent être éliminés et empêchés en CI.
17. Le chiffrement au repos est bien conçu dans l'ensemble (clé aléatoire, SQLCipher, Argon2id, enveloppes ChaCha20-Poly1305, permissions `0600`, trousseau opt-in). C'est un point fort à conserver et tester davantage.

#### Proactivité et autonomie

1. « Reprendre le travail » choisit simplement le fichier/document/projet au mtime le plus récent des trois derniers jours. Cela ne prouve pas que l'utilisateur travaillait dessus, ni hier, ni qu'il veut le reprendre.
2. Le brief est supprimé si une conversation utilisateur existe déjà ce jour-là, même si elle n'a aucun rapport avec le démarrage de journée.
3. Le bilan « tâches terminées aujourd'hui » sélectionne les dernières tâches `done` de tous les temps : la table n'a pas de `completed_at`.
4. « Mails non traités » signifie en réalité « mails récents », faute d'état lu/répondu/archivé.
5. Les engagements ouverts ne possèdent pas de boucle de correction/validation fiable.
6. Le moteur n'observe ni application active, ni documents réellement ouverts, ni sessions d'éditeur, ni historique d'activité. Il ne peut donc pas produire la suggestion contextuelle souhaitée avec une confiance acceptable.
7. Les règles de fond sont compilées vers quelques conditions figées (CPU, disque, batterie) ; une règle générique devient souvent un contrôle quotidien sans sémantique opérationnelle réelle.
8. Les `action_modifier` sont injectés dans le prompt mais pas nécessairement appliqués de façon déterministe.
9. Pas de feedback « utile / pas utile », snooze, fréquence par type, apprentissage des moments opportuns ou raison détaillée avec preuves.
10. Le moteur ne fonctionne que lorsque l'application est lancée et déverrouillée ; ce n'est pas un agent de fond omniprésent au niveau système.

## Capacités réelles aujourd'hui

| Domaine | Ce que Syn sait réellement faire | Limites importantes |
|---|---|---|
| Conversation | Répondre avec un LLM Ollama local, historique court, résumé long | Ollama requis, modèles modestes, aucune preuve de taux de réussite |
| Fichiers | Indexer/rechercher plusieurs formats, ouvrir, déplacer, proposer un rangement avec undo | couverture partielle, pas d'OCR, projets résumés, index non scalable |
| Mail Apple | Rechercher une partie du stock local, créer brouillon, envoyer mail simple après confirmation | backlog incomplet, adresse/personne fragile, pas de fils/pièces jointes/CC/BCC |
| Calendrier Apple | Lire une plage et créer un événement simple | miroir partiel, pas d'update/delete agent, pas d'invités/récurrence |
| Rappels | Intention de miroir bidirectionnel | cassé par le statut `authorized`/`granted` |
| Messages | Indexer jusqu'à 4 000 textes locaux | pas d'outil dédié, groupes/attachments ignorés, pas d'envoi |
| Contacts | Importer et stocker nom/email/téléphone/relation | pas de synchro continue ni résolution robuste des doublons |
| Photos | Rechercher des fichiers image via nom/EXIF | pas PhotoKit, scène, OCR ou visages |
| Écran | Capture manuelle de l'écran principal + OCR local | pas fenêtre exacte garantie, pas vision, pas interaction UI |
| Système | CPU, mémoire, disques, batterie, processus, quelques alertes | observation seulement, températures variables selon OS, peu d'actions |
| Tâches/mémoire | Créer/lister/compléter tâches, mémoriser un fait | cohérence native fragile, pas de modèle temporel/relations riche |
| Règles | Ton/tutoiement, quelques déclencheurs système | langage libre largement surpromis |
| Cloud | Obtenir certains jetons OAuth | aucune synchronisation ou action métier |
| Voix | Lecture via `say` | dictée/micro non disponibles |

## Ce que Syn ne peut pas honnêtement promettre aujourd'hui

- « Je vois tout ce qui se passe sur ton Mac. »
- « Je peux agir dans n'importe quelle application. »
- « Je retrouve n'importe quel document, même scanné ou mal nommé. »
- « Je sais sur quoi tu travaillais hier. »
- « Je connais l'état réel de tes mails et de tes conversations. »
- « Google, Microsoft, Slack et GitHub sont connectés à mon intelligence. »
- « Je peux accomplir de façon autonome un workflow long entre plusieurs services. »
- « Tout est embarqué dans Syn. » Ollama reste un prérequis de développement externe.
- « Rien ne sort jamais de l'appareil. » Les connexions OAuth sont des sorties explicites.
- « Une annulation restaure toujours l'état externe. » Ce n'est pas vrai pour les brouillons Mail et Rappels.

## Intelligence : le bon modèle mental

Syn ne deviendra pas omniscient parce qu'un modèle plus gros est installé. Son intelligence utile est le produit de cinq couches :

1. **Perception** : connecteurs exhaustifs, fiables et à jour.
2. **Mémoire** : modèle de données temporel, personnes, projets, objets et provenance.
3. **Recherche** : rappel élevé, reranking, résolution d'entités et présentation des alternatives.
4. **Décision** : routeur déterministe pour les intentions critiques, planner borné pour le reste.
5. **Action** : outils réels, transactionnels, vérifiables, annulables et soumis à une politique.

Le LLM actuel est surtout un interprète et un rédacteur. Il ne possède ni accès intrinsèque au Mac, ni mémoire propre, ni capacité d'action hors du catalogue. Le catalogue actuel contient environ 18 outils, dont plusieurs lectures. L'« omnipotence » ne peut venir que d'un catalogue beaucoup plus large et d'une couche d'automatisation OS, pas du prompt.

Pour un assistant desktop convaincant, la règle devrait être : **déterministe pour reconnaître, autoriser et vérifier ; probabiliste pour comprendre, planifier et rédiger**. Les invariants critiques — destinataire, fichier cible, portée d'une suppression, état final — doivent être vérifiés hors modèle.

## Les deux cas d'usage de référence

### « Retrouve mon cours sur la PSSI »

Comportement cible :

1. classifier la demande en `file_lookup` sans consulter d'abord le contexte du projet courant ;
2. extraire concepts et variantes (`PSSI`, politique de sécurité des systèmes d'information, cours, support, PDF, diaporama) ;
3. chercher titre, chemin, métadonnées, contenu OCR/texte et historique d'ouverture ;
4. reranker avec type de document, proximité sémantique, origine « cours », date plausible et activité passée ;
5. afficher 1 à 5 cartes fichiers avec aperçu, emplacement, date, raison du match et bouton Quick Look/Ouvrir ;
6. si confiance forte, ouvrir/fournir le premier tout en montrant les alternatives ;
7. si index incomplet ou aucun résultat, expliquer la couverture et proposer une recherche élargie — jamais répondre avec un projet hors sujet.

Critères d'acceptation : Recall@5 > 95 % sur corpus cible, aucun résultat d'un autre domaine si un candidat pertinent existe, réponse vide honnête, temps médian < 2 s sur index chaud.

### « Souhaitez-vous reprendre le projet d'hier ? »

Il faut créer une **timeline d'activité locale** plutôt que regarder le mtime d'un dossier : application au premier plan, fenêtres/documents ouverts via API d'accessibilité, événements FSEvents, répertoire Git, branche/commits, session d'éditeur et durée active. Stocker uniquement les événements minimaux nécessaires, chiffrés, avec rétention réglable.

Le moteur peut alors détecter une session : « VS Code + projet Aberration, 52 min, trois fichiers modifiés, branche `feature/x`, activité arrêtée à 18:12 ». La suggestion devient vérifiable : « Reprendre Aberration ? Ouvrir le projet et la dernière conversation Syn associée. » Elle doit avoir un bouton d'action et un bouton « ne plus suggérer ceci ».

## Audit fonctionnel par surface

### Onboarding

- L'email est demandé sans expliquer son utilité alors que le compte est local et l'email optionnel côté backend.
- Pas de confirmation du mot de passe, indicateur de robustesse, avertissement Caps Lock ni validation claire avant soumission.
- La phrase de récupération peut être validée d'un clic sans test de restitution, copie sécurisée ou impression.
- L'étape Contacts demande un engagement important avant que la valeur du produit soit démontrée.
- Les connecteurs cloud de développement sont proposés avant d'être fonctionnels.
- L'étape finale parle encore de « dossiers à indexer », en contradiction avec la décision d'accès global ; le sélecteur dossier par dossier subsiste.
- L'utilisateur peut ouvrir Syn sans modèle prêt ni première indexation terminée, puis rencontrer un produit vide/dégradé sans guidage fort.
- Le rendu observé coupe le haut du titre à l'étape Contacts ; progression par points non libellés et peu accessible.

### Accueil et conversation

- Accueil élégant mais trop vide ; aucune suggestion d'exemples lorsque le brief est vide.
- Les états index vide/en cours/LLM indisponible ne sont pas assez dominants.
- La conversation expose des étapes techniques mais pas toujours un résultat métier clair.
- Les erreurs d'ouverture de source sont souvent avalées silencieusement.
- Les résultats devraient être des cartes structurées (fichier/mail/personne/événement) plutôt qu'un texte dépendant du modèle.
- Pas de correction/édition d'une action en attente ; seulement confirmer/refuser.
- Pièces jointes et dictée sont visibles mais désactivées, ce qui souligne l'incomplétude au cœur de la barre.
- Les sessions et projets de conversation ne constituent pas encore un vrai espace de travail : pas de fichiers épinglés, objectifs, état, résumé vérifiable ou prochaines actions.

### Connaissances et activité

- La page Connaissances mélange « faits explicitement appris », items de conversations et données indexées sans expliquer confiance, provenance et durée de conservation.
- Faire oublier un fichier ajoute une exclusion durable, mais l'impact et la façon de le réautoriser ne sont pas visibles dans la ligne.
- Pas d'aperçu du contenu, qualité d'extraction, erreur ou raison d'indexation.
- Activité montre des noms techniques d'outils et du JSON au survol ; utile au développeur, peu compréhensible au grand public.
- Le journal n'offre ni filtre, recherche, export, regroupement par workflow ni détail des observations.
- « Annuler » n'indique pas quand l'annulation est partielle ou impossible côté service externe.

### Connecteurs, programmations et réglages

- Les notions autorisation OS, connecteur, synchronisation et disponibilité sont confondues.
- Apple est toujours marqué connecté même si Mail n'est pas lisible ; les services Apple devraient avoir chacun leur état réel.
- La page Connecteurs interroge permissions et états toutes les trois secondes, y compris des vérifications disque ; préférer événements et cadence raisonnable.
- Les règles automatiques affichent les conditions techniques (`cpu.pct>85`) au lieu d'une formulation utilisateur et ne montrent pas ce qui sera réellement fait.
- « Gardien système actif » ne possède pas de toggle direct dans la page.
- Mode travail ne contrôle que les notifications Syn, pas les interruptions de l'OS ou des autres applications ; le texte devrait préciser la portée.
- Mode économie met l'indexeur en pause mais les autres boucles continuent ; « réduit l'activité de fond » est trop large.
- Plusieurs réglages « bientôt » encombrent un produit déjà incomplet ; mieux vaut masquer ou isoler une rubrique laboratoire.

## Audit esthétique et accessibilité

### Points réussis

- Identité sombre cohérente, discrète, proche des conventions macOS.
- Espacements, cartes et icônes globalement homogènes.
- Confirmation d'action bien séparée visuellement et non précochée.
- Réduction des animations et option de texte agrandi déjà prévues.
- L'interface évite le bruit et conserve une personnalité sobre.

### Défauts

1. Taille de base à 13 px, beaucoup de textes à 12, 11 ou 10,5 px : trop petit pour une application riche en contexte.
2. `--text-tertiary: #77777d` sur fonds sombres produit un contraste faible ; plusieurs placeholders, métadonnées et icônes deviennent difficiles à lire.
3. Les `input` ont `outline: none` et aucune règle `:focus-visible` n'existe : navigation clavier non perceptible.
4. Beaucoup de boutons icône ont des cibles autour de 22–28 px, inférieures à une cible confortable et souvent au minimum WCAG 2.2 de 24 px sans garantie d'espacement.
5. De nombreux contrôles n'ont qu'un `title`, pas de nom accessible robuste. Les toggles n'ont pas de `aria-label` ni association au texte voisin.
6. Modales et popovers ne montrent pas de focus trap, retour du focus, fermeture Escape ou sémantique `dialog`.
7. Pas de thème clair ni respect natif de `prefers-color-scheme`; ce n'est pas seulement esthétique, c'est une préférence d'accessibilité.
8. Mise en page peu responsive avec minimum 960×640 ; les deux colonnes et petits textes souffriront au zoom 200 %.
9. L'onboarding utilise une image de fond d'environ 11 Mo, disproportionnée pour un décor statique et incluse dans le bundle.
10. Les états vides occupent de grands espaces sans pédagogie, exemples ni actions de départ.
11. Le français alterne tutoiement/vouvoiement dans des chaînes non toutes templatisées ; la personnalisation de voix ne couvre qu'un sous-ensemble.
12. Plusieurs textes sont imprécis ou fautifs, par exemple la démo « chez les coiffeur ».

Recommandation visuelle : conserver l'identité, passer le corps à 14–15 px, relever les contrastes, utiliser des actions textuelles quand l'icône n'est pas évidente, créer une hiérarchie forte `état → résultat → action`, et construire des cartes riches pour les objets plutôt qu'une succession de lignes grises.

## Dette d'ingénierie et qualité

- Aucun script `test`, `lint`, `typecheck` ou `format` dans `package.json`.
- Aucun test de composant Solid, de navigation, d'accessibilité ou de capture visuelle de régression.
- Aucun pipeline CI.
- Clippy échoue avec `-D warnings` (retour inutile, réassignations après `Default`, ordre module/tests).
- README annonce 36 tests alors que 40 ont été exécutés : documentation déjà désynchronisée.
- Documentation technique dupliquée (`copie.md`), archive ZIP et artefacts de dossier ; la source de vérité n'est pas nette.
- La documentation historique prescrit le moindre privilège dossier par dossier alors que la décision produit actuelle est l'accès global. Il faut versionner/archiver l'ancienne règle au lieu de conserver deux prescriptions actives.
- Plusieurs commentaires `audit §...` décrivent des corrections ponctuelles, mais il manque un registre de décisions/ADR et des issues reliées à des tests.
- Pas de télémétrie produit, ce qui est cohérent avec la souveraineté, mais pas davantage de métriques locales exportables pour comprendre taux de réussite, latence, couverture et erreurs.
- Migrations avec `ALTER TABLE` non idempotents hors suivi `_migrations`; la discipline doit être testée sur upgrade réel, rollback impossible et base corrompue.
- `(source, source_ref)` n'est pas unique dans `items`, alors que l'upsert l'assume ; une concurrence peut créer des doublons.
- `persist_turn` calcule `MAX(turn)+1` puis insère sans transaction dédiée ; deux requêtes simultanées sur une session peuvent entrer en collision.

## Architecture cible pour la promesse « omnisciente et omnipotente »

### 1. Graphe de contexte local

Créer des entités stables `Document`, `Project`, `Person`, `Message`, `Thread`, `Event`, `Task`, `App`, `Window`, `ActivitySession`, reliées par provenance et temps. Séparer :

- vérité source ;
- index dérivé ;
- faits explicitement validés ;
- inférences avec score/confiance ;
- suggestions proposées ;
- actions exécutées et état final vérifié.

### 2. Bus d'activité

Ajouter des événements locaux normalisés : fichier ouvert/modifié, application au premier plan, projet détecté, mail reçu/lu/répondu, événement imminent, conversation associée. Déduplication, rétention, chiffrement et pause visibles. Ne pas enregistrer continuellement le contenu de l'écran : préférer métadonnées et Accessibility API, capture ponctuelle seulement si nécessaire.

### 3. Retrieval de production

- index lexical FTS5/BM25 avec tokenisation française ;
- index vectoriel ANN local ;
- filtres source/type/date/personne/projet avant scoring ;
- réécriture multi-requêtes et expansion d'acronymes ;
- OCR Apple Vision pour images/PDF ;
- reranker local ;
- calibration de confiance et abstention ;
- citations natives et résultats structurés indépendants du texte du LLM.

### 4. Routeur et planner

Mettre les flux à haute fréquence dans des machines d'état déterministes : recherche fichier, envoyer mail, créer événement, déplacer/ranger, créer tâche. Le modèle remplit un schéma ; le backend valide présence, provenance et ambiguïtés. Réserver le planner agentique aux demandes composées, avec budget, checkpoint, reprise et journal complet.

### 5. Couche d'action desktop

Trois niveaux :

- API native/service en priorité ;
- Apple Shortcuts, AppleScript/ScriptingBridge et URL schemes ensuite ;
- Accessibility API/UI automation en dernier recours, avec cible affichée, vérification avant/après et confirmation adaptée.

Chaque action doit déclarer préconditions, effets, risque, idempotence, undo/compensation, preuve de succès, timeout et droits requis.

### 6. Politique d'autonomie

Conserver le plancher actuel, qui est un bon principe. Ajouter politiques par domaine/personne/service, expiration des confirmations, limites de fréquence/coût, simulations, transactions compensatoires et « toujours demander pour X ». La confirmation doit afficher destinataire réel résolu, compte source, contenu complet, pièces jointes et conséquences.

## Fonctionnalités et services à ajouter

Ordre conseillé par valeur produit :

1. **Recherche universelle fiable** : OCR, Quick Look, résultats structurés, couverture et indexation des projets.
2. **Mail complet** : threads, lu/non lu, réponses, pièces jointes, CC/BCC, choix du compte, suivi « traité ».
3. **Timeline d'activité et reprise de travail** : applications, documents, projets et sessions.
4. **Notes et connaissance** : Apple Notes, PDF annotés, pages web sauvegardées, Notion/Obsidian si choisi.
5. **Cloud réellement métier** : Google Workspace puis Microsoft 365, avec Drive/OneDrive + Mail + Calendar cohérents avant d'ajouter d'autres logos.
6. **Tâches** : Rappels réparé, puis Todoist/Things/Asana/Linear selon cible utilisateur.
7. **Communication** : Messages/Slack/Teams avec recherche de fils, résumé et rédaction ; envoi toujours contrôlé.
8. **Navigateur** : Safari/Chrome onglets, historique local, lecture de page et ouverture ; actions web seulement avec garde stricte.
9. **Photos** : PhotoKit, OCR, scènes, albums ; visages uniquement opt-in séparé et juridiquement cadré.
10. **Voix** : Whisper local, mode conversation court, confirmation vocale + visuelle pour actions externes.
11. **Réunions** : capture/transcription locale, consentement visible, extraction de décisions proposée et validée.
12. **Automatisations** : modèles « quand X, proposer Y », jamais exécution opaque ; galerie de recettes inspectables.
13. **Santé numérique** : synthèse de charge, doublons, stockage, batterie, mises à jour — sans prétendre diagnostiquer au-delà des signaux disponibles.
14. **Mode développeur** : Git, IDE, terminal, issues/PR, builds et tests, isolé du mode personnel pour éviter les contaminations de contexte comme Aberration/quittance.

Ne pas connecter vingt services superficiellement. Mieux vaut trois intégrations profondes, cohérentes et vérifiées que vingt tuiles OAuth sans données.

## Feuille de route recommandée

### Phase A — fiabilisation immédiate (1–2 semaines)

- corriger Rappels `granted` ;
- supprimer l'écriture automatique d'engagements depuis les mails ;
- paginer/réconcilier Mail, Messages, Calendrier et Files ;
- effacer les embeddings obsolètes ;
- séparer les capabilities Tauri `main`/`bar` ;
- ajouter expiration/édition des confirmations ;
- corriger les undo Mail/Rappels ou les déclarer non annulables ;
- mettre à jour Vite/esbuild, rendre Clippy vert ;
- CI : fmt, clippy, tests Rust, typecheck, build, audit dépendances ;
- masquer les connecteurs métier non implémentés.

### Phase B — tenir les deux promesses cœur (3–6 semaines)

- benchmark de recherche versionné ;
- FTS5 + index ANN + reranker ;
- OCR et formats bureautiques manquants prioritaires ;
- index enfant des projets ;
- cartes de résultats/Quick Look/alternatives ;
- routeurs déterministes pour fichier/mail/calendrier ;
- tests E2E réels des formulations françaises et parcours confirmation.

### Phase C — contexte réellement utile (6–12 semaines)

- timeline d'activité chiffrée et réglable ;
- entité projet et association conversations/fichiers/apps ;
- reprise de travail basée sur session, pas mtime ;
- état mail traité/répondu et tâches avec timestamps ;
- feedback sur suggestions et apprentissage local ;
- Apple Notes/Photos/Browser en lecture.

### Phase D — action et interconnexion (ensuite)

- Google Workspace complet, puis Microsoft 365 ;
- mail riche et calendriers riches ;
- Shortcuts/Accessibility avec vérification d'état ;
- workflows multi-outils transactionnels ;
- runtime LLM embarqué, modèles évalués, option cloud réellement consentie ;
- signature, notarisation et updater signé avant distribution.

## Critères de sortie d'une vraie V1

- Les 20 intentions principales ont chacune un taux de succès mesuré et un seuil bloquant.
- Recherche document : Recall@5 ≥ 95 % sur corpus représentatif ; zéro réponse hors domaine quand un bon résultat existe.
- Envoi mail : 100 % des noms passent par résolution de contact ; 0 adresse synthétique ; contenu absent provoque toujours une question ; aperçu complet avant envoi.
- Toutes les sources ont couverture, fraîcheur, erreurs et état de synchronisation visibles.
- Toutes les actions ont preuve de résultat et undo réel ou mention explicite « non annulable ».
- Aucun connecteur n'est marqué connecté avant un test métier réussi.
- Verrouillage annule/termine toutes les tâches sensibles et purge les clés en mémoire.
- Capabilities Tauri minimales par fenêtre ; redirections réseau contrôlées.
- CI verte, audits dépendances sans haute/critique, app signée/notarisée, mises à jour signées.
- Parcours clavier complet, focus visible, contraste WCAG AA, zoom 200 % sans perte.
- Documentation produit générée depuis une matrice de capacités effective, sans doublons ni promesses mortes.

## Références externes utilisées

- Tauri confirme que les capabilities bornent les privilèges par fenêtre et que les commandes enregistrées sont accessibles par défaut à toutes les webviews si elles ne sont pas déclarées : <https://v2.tauri.app/security/capabilities/>.
- Tauri recommande signature/notarisation pour la distribution macOS : <https://v2.tauri.app/distribute/>.
- L'updater Tauri impose la signature cryptographique des mises à jour : <https://v2.tauri.app/plugin/updater/>.
- Apple rappelle que le sandbox sert à contenir les dommages d'une application compromise. Syn peut choisir de ne pas l'utiliser pour sa promesse d'accès global, mais doit compenser ce rayon d'impact par un durcissement supérieur : <https://developer.apple.com/documentation/security/app-sandbox>.
- RFC 8252 définit PKCE et les redirections loopback pour les applications natives : <https://datatracker.ietf.org/doc/html/rfc8252>.
- RFC 9700 consolide les bonnes pratiques OAuth actuelles : <https://www.rfc-editor.org/rfc/rfc9700.html>.
- OWASP recommande de considérer le contenu récupéré comme non fiable et de placer les contrôles d'action hors du modèle : <https://cheatsheetseries.owasp.org/cheatsheets/LLM_Prompt_Injection_Prevention_Cheat_Sheet.html>.
- WCAG 2.2 fixe notamment les exigences de contraste, focus, redimensionnement et taille minimale des cibles : <https://www.w3.org/TR/WCAG22/>.

## Conclusion

La vision de Syn est crédible, mais elle demande d'investir d'abord dans les « sens », la mémoire structurée, la recherche et les outils — pas uniquement dans le modèle. Le dépôt possède déjà deux fondations différenciantes : le local chiffré et une porte de confirmation hors modèle. Il faut maintenant rendre le produit **honnête sur son état**, transformer chaque connecteur en capacité profonde et mesurée, et traiter la recherche universelle comme un moteur de recherche à part entière.

Le meilleur cap n'est pas « Syn sait tout ». C'est : **Syn sait exactement ce qu'il a vu, d'où cela vient, à quel point il en est sûr, ce qu'il peut réellement faire, et il vérifie qu'il l'a bien fait.** Une fois ces garanties en place, l'impression d'omniscience et d'omnipotence émergera naturellement de la continuité et de la fiabilité.
