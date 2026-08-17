//! Corpus d'évaluation du routage d'intention.
//!
//! Règle de construction : **aucune formulation de ce corpus n'a servi à écrire
//! le code de routage**. Les cas mélangent registres (familier, soutenu,
//! elliptique, télégraphique), langues (français, anglais), et tournures qui
//! n'emploient volontairement pas les verbes « attendus ». C'est la seule façon
//! de mesurer si Syn comprend une demande ou s'il reconnaît un gabarit.
//!
//! Le corpus sert à produire un TAUX D'ERREUR mesuré, pas une impression.

/// Ce que Syn doit faire de la demande. Volontairement grossier : on mesure
/// l'aiguillage, pas la qualité de la réponse finale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Chercher un document, où qu'il soit.
    FileSearch,
    /// Chercher un document en se limitant à un fournisseur nommé.
    FileSearchGoogle,
    FileSearchMicrosoft,
    FileSearchLocal,
    /// Composer un message à une personne.
    MailCompose,
    /// Lire l'état de la machine.
    DeviceDiagnostic,
    /// Créer un nouveau document.
    DocumentCreate,
    /// Tout le reste : conversation, question de fond, action outillée.
    Conversation,
}

pub struct Case {
    pub text: &'static str,
    pub expected: Route,
    /// Pourquoi ce cas est piégeux — sert au rapport d'erreurs.
    pub note: &'static str,
}

const fn case(text: &'static str, expected: Route, note: &'static str) -> Case {
    Case {
        text,
        expected,
        note,
    }
}

pub const CORPUS: &[Case] = &[
    // ——— Recherche documentaire, français, sans verbe de recherche ———
    case(
        "Le Jeu de la Vie, tu l'as quelque part ?",
        Route::FileSearch,
        "aucun verbe de recherche, aucun mot « document »",
    ),
    case(
        "j'ai besoin de la convention collective Syntec",
        Route::FileSearch,
        "besoin de, pas « cherche »",
    ),
    case(
        "il me manque le compte rendu de la réunion du 12 mars",
        Route::FileSearch,
        "formulation par le manque",
    ),
    case(
        "Où j'ai foutu mon attestation d'assurance ?",
        Route::FileSearch,
        "registre familier",
    ),
    case(
        "la lettre de motivation de Camille, elle est où",
        Route::FileSearch,
        "dislocation, pas de ponctuation",
    ),
    case(
        "tu te souviens du dossier sur les xénobots ?",
        Route::FileSearch,
        "formulé comme une question de mémoire",
    ),
    case(
        "ressors-moi le bilan comptable 2025",
        Route::FileSearch,
        "verbe attendu — cas facile de contrôle",
    ),
    case(
        "mon bail",
        Route::FileSearch,
        "télégraphique, deux mots",
    ),
    case(
        "faudrait que je relise le rapport Ducasse avant demain",
        Route::FileSearch,
        "intention indirecte",
    ),
    case(
        "passe-moi la fiche de paie de novembre",
        Route::FileSearch,
        "« passe-moi » n'est pas dans la liste des verbes",
    ),
    // ——— Recherche documentaire, anglais ———
    case(
        "Where's my lease agreement?",
        Route::FileSearch,
        "anglais : aucune porte française ne s'ouvre",
    ),
    case(
        "I need the Q3 revenue forecast",
        Route::FileSearch,
        "anglais, pas de verbe de recherche",
    ),
    case(
        "pull up the Syntec collective agreement",
        Route::FileSearch,
        "phrasal verb anglais",
    ),
    case(
        "do you still have Camille's cover letter?",
        Route::FileSearch,
        "anglais, question de possession",
    ),
    // ——— Portée explicite : un fournisseur est nommé ———
    case(
        "Le Jeu de la Vie, il est sur mon Drive normalement",
        Route::FileSearchGoogle,
        "fournisseur nommé sans « google docs »",
    ),
    case(
        "regarde côté Google Docs pour le rapport de stage",
        Route::FileSearchGoogle,
        "fournisseur nommé, verbe inattendu",
    ),
    case(
        "check SharePoint for the onboarding deck",
        Route::FileSearchMicrosoft,
        "anglais + fournisseur Microsoft",
    ),
    case(
        "j'ai mis le budget quelque part dans OneDrive",
        Route::FileSearchMicrosoft,
        "fournisseur nommé, formulation par le souvenir",
    ),
    case(
        "le contrat doit être sur le disque, pas dans le cloud",
        Route::FileSearchLocal,
        "portée locale exprimée par exclusion",
    ),
    case(
        "cherche uniquement en local stp",
        Route::FileSearchLocal,
        "portée locale explicite",
    ),
    // ——— Composition de message ———
    case(
        "préviens Marie que je serai en retard",
        Route::MailCompose,
        "ni « mail » ni « envoie »",
    ),
    case(
        "il faut que je réponde à Thomas au sujet du devis",
        Route::MailCompose,
        "intention indirecte, sans « mail »",
    ),
    case(
        "drop Sarah a note about tomorrow's meeting",
        Route::MailCompose,
        "anglais",
    ),
    case(
        "envoie un mail à paul@example.com pour confirmer",
        Route::MailCompose,
        "formulation attendue — cas de contrôle",
    ),
    case(
        "remercie Jean pour son retour",
        Route::MailCompose,
        "verbe d'acte de langage, aucun mot de messagerie",
    ),
    // ——— État de la machine ———
    case(
        "ça rame sévère depuis ce matin",
        Route::DeviceDiagnostic,
        "plainte, aucun mot technique",
    ),
    case(
        "il reste combien de place ?",
        Route::DeviceDiagnostic,
        "elliptique, « place » n'est pas « stockage »",
    ),
    case(
        "why is my fan so loud?",
        Route::DeviceDiagnostic,
        "anglais, symptôme physique",
    ),
    case(
        "quelle est la charge de mon processeur",
        Route::DeviceDiagnostic,
        "formulation attendue — cas de contrôle",
    ),
    case(
        "mon ordi est brûlant",
        Route::DeviceDiagnostic,
        "« ordi » abrégé, « brûlant » absent des listes",
    ),
    // ——— Création de document ———
    case(
        "note-moi les points de la réunion dans un fichier",
        Route::DocumentCreate,
        "création exprimée sans « crée »",
    ),
    case(
        "rédige un compte rendu de ce qu'on vient de dire",
        Route::DocumentCreate,
        "verbe de rédaction",
    ),
    case(
        "draft a one-pager about the Syntec agreement",
        Route::DocumentCreate,
        "anglais",
    ),
    case(
        "j'aimerais un tableau récapitulatif des dépenses",
        Route::DocumentCreate,
        "souhait, pas d'impératif",
    ),
    // ——— Conversation : ne DOIT pas déclencher de recherche ———
    case(
        "Explique-moi ce qu'est le Jeu de la Vie de Conway",
        Route::Conversation,
        "question de culture générale, même sujet qu'une recherche",
    ),
    case(
        "merci, c'est parfait",
        Route::Conversation,
        "clôture",
    ),
    case(
        "tu penses que c'est une bonne idée ?",
        Route::Conversation,
        "avis",
    ),
    case(
        "what can you do exactly?",
        Route::Conversation,
        "méta, en anglais",
    ),
    case(
        "raconte-moi une blague",
        Route::Conversation,
        "hors périmètre documentaire",
    ),
];

/// Jeu de VALIDATION — écrit après la mise au point, mesuré une seule fois,
/// jamais utilisé pour ajuster le prompt ni les exemples de calibrage.
///
/// C'est le seul chiffre honnête : le corpus ci-dessus a servi au réglage, il
/// mesure donc aussi ce que le réglage lui a appris. Celui-ci mesure ce que Syn
/// comprend d'une demande qu'aucune étape de conception n'a vue.
pub const VALIDATION: &[Case] = &[
    case("le PV d'assemblée générale, il est passé où", Route::FileSearch, "sigle, dislocation"),
    case("j'arrive plus à mettre la main sur mon relevé de notes", Route::FileSearch, "périphrase"),
    case("t'aurais pas gardé la notice du lave-vaisselle ?", Route::FileSearch, "élision familière"),
    case("Kannst du die Rechnung von März finden?", Route::FileSearch, "allemand : troisième langue"),
    case("show me last year's tax return", Route::FileSearch, "anglais impératif"),
    case("la clause de non-concurrence est dans le contrat signé", Route::FileSearch, "affirmation qui présuppose la recherche"),
    case("il est sur Sharepoint le référentiel qualité", Route::FileSearchMicrosoft, "fournisseur en fin de phrase"),
    case("va voir dans Google Sheets le suivi des heures", Route::FileSearchGoogle, "fournisseur nommé"),
    case("uniquement ce qui est stocké sur la machine", Route::FileSearchLocal, "portée locale sans nom de fichier"),
    case("signale à Nadia que le colis est arrivé", Route::MailCompose, "acte de parole, destinataire"),
    case("faut que je décline l'invitation de M. Perrin", Route::MailCompose, "intention rapportée"),
    case("apologise to the client for the delay", Route::MailCompose, "anglais, acte de parole"),
    case("l'écran scintille par moments", Route::DeviceDiagnostic, "symptôme matériel inédit"),
    case("mon disque est plein à ras bord", Route::DeviceDiagnostic, "expression imagée"),
    case("is the battery holding up?", Route::DeviceDiagnostic, "anglais familier"),
    case("prépare-moi un modèle de lettre de résiliation", Route::DocumentCreate, "production"),
    case("il me faudrait un pense-bête avec ces trois points", Route::DocumentCreate, "production, mot rare"),
    case("turn this into a proper memo", Route::DocumentCreate, "anglais, transformation"),
    case("qu'est-ce qu'une clause de non-concurrence ?", Route::Conversation, "savoir, sujet identique à un cas de recherche"),
    case("t'es sûr de toi là ?", Route::Conversation, "mise en doute"),
    case("bon, on verra demain", Route::Conversation, "clôture floue"),
    case("how do you store my data?", Route::Conversation, "méta, anglais"),
    case("resume ce qu'on a dit", Route::Conversation, "résumé oral, pas de fichier demandé"),
];
