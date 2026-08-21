// Mode démo (hors Tauri, ex. navigateur) : mock IPC avec les données factices
// des maquettes. Sert à la prévisualisation design — jamais utilisé dans l'app.

const qs = new URLSearchParams(location.search);
const demoScreen = qs.get("screen") ?? "app";

const SETTINGS = {
  autonomy: "assiste",
  startup_brief_enabled: true,
  brief_floor_hour: 7,
  brief_sections: ["events", "tasks", "commitments", "mails", "birthdays", "continue"],
  daily_wrap_enabled: true,
  daily_wrap_hour: 18,
  autostart: false,
  voice: { formality: "vous", address_form: "Monsieur", extras: [] },
  theme: "dark",
  answer_language: "auto",
  voice_input_enabled: false,
  voice_output_enabled: false,
  bar_shortcut: "Alt+Space",
  reduce_motion: false,
  large_text: false,
  notifications_enabled: true,
  notifications_muted: false,
  notification_sound: true,
  notification_min_priority: "info",
  notify_briefs: true,
  notify_events: true,
  notify_commitments: true,
  notify_system: true,
  notify_rules: true,
  notify_reflexes: true,
  work_notification_policy: "urgent",
  cloud_escalation: false,
    sensitive_consent: true,
    files_full_access_requested: false,
  rarity_budget: 5,
  guardian_disk_pct: 5,
  guardian_temp_c: 90,
  work_mode: false,
  eco_mode: false,
  indexing_paused: false,
  tier: "standard",
  chat_model: "llama3.1:latest",
  embed_model: "nomic-embed-text",
  ollama_url: "http://127.0.0.1:11434",
  onboarding_done: demoScreen === "app",
  last_brief_date: "",
  last_wrap_date: "",
};

const BRIEF = {
  greeting: "Bonjour Monsieur,",
  items: [
    {
      icon: "message",
      text: "Vous avez un message de Maman au sujet de votre logement, qui attend votre réponse",
      sub: null,
      source_ref: null,
      kind: "mail",
    },
    {
      icon: "calendar",
      text: "Aujourd'hui vous avez rendez-vous chez les coiffeur à 17h30",
      sub: null,
      source_ref: null,
      kind: "event",
    },
    {
      icon: "gmail",
      text: "Mail Gmail concernant votre réservation de billets de train Paris → Lyon",
      sub: null,
      source_ref: null,
      kind: "mail",
    },
  ],
  chips: [
    { icon: "cake", text: "C'est l'anniversaire de Mathéo A. aujourd'hui", source_ref: null },
    { icon: "file", text: "Continuer de travailler sur “Rapport_IAM.docx”", source_ref: null },
  ],
  empty: false,
  generated_at: 0,
};

const RULES = [
  { id: "r1", text: "#Vouvoie-moi et appelle-moi “Monsieur”", kind: "style", status: "active", priority: 0, params: null, reason: null, created_at: 0 },
  { id: "r2", text: "#Surveille régulièrement les performances de mon ordinateur", kind: "standing", status: "active", priority: 0, params: null, reason: null, created_at: 0 },
  { id: "r3", text: "#Dès que tu envoies un message à ma mère, ajoute un emoji de cœur", kind: "action_modifier", status: "active", priority: 0, params: null, reason: null, created_at: 0 },
];

const CONTACTS = [
  { name: "Maman", phone: "06 07 08 09 10", email: null },
  { name: "Papa", phone: "+33 6 10 11 12 13", email: null },
  { name: "Jean", phone: "07 12 39 48 57", email: null },
  { name: "Jean-Jean", phone: "06 90 80 70 60", email: null },
  { name: "Jean-Jacques", phone: "06 11 22 33 44", email: null },
];

export async function demoInvoke(cmd: string, _args?: any): Promise<any> {
  switch (cmd) {
    case "app_status":
      return {
        initialized: demoScreen !== "onboarding",
        unlocked: demoScreen === "app" || demoScreen === "onboarding-late",
        onboarding_done: demoScreen === "app",
        email: "paul@exemple.fr",
        keychain: true,
        keychain_available: true,
      };
    case "get_settings":
    case "set_settings":
      return SETTINGS;
    case "get_startup_brief":
      return BRIEF;
    case "rules_list":
      return RULES;
    case "rules_add":
      return { status: "active", kind: "style", reason: null, id: "rX", conflict_with: null };
    case "people_os_preview":
      return CONTACTS;
    case "list_pending_actions":
    case "list_sessions":
    case "unknowns_pending":
    case "list_surfacings":
    case "access_log_list":
    case "list_actions":
      return [];
    case "people_list":
      return CONTACTS.map((c, i) => ({ id: String(i), name: c.name, relationship: i < 2 ? (i === 0 ? "mère" : "père") : null, birthday: i === 0 ? "03-14" : null }));
    case "list_triggers":
      return [
        { id: "t1", type: "threshold", condition: "cpu.pct>85", priority: "important", reason_template: "Règle active : #Surveille régulièrement les performances de mon ordinateur", action: "notify", source: "rule", enabled: true, last_fired: null, rule_text: "#Surveille régulièrement les performances de mon ordinateur" },
        { id: "sys.mail_sans_reponse", type: "context", condition: "mail.sans_reponse", priority: "important", reason_template: "Message resté sans réponse", action: "notify", source: "system", enabled: true, last_fired: Date.now() / 1000 - 7200, rule_text: null },
        { id: "sys.preparation_reunion", type: "context", condition: "agenda.reunion_imminente", priority: "important", reason_template: "Réunion imminente, avec de quoi la préparer", action: "notify", source: "system", enabled: true, last_fired: null, rule_text: null },
        { id: "sys.engagement_oublie", type: "context", condition: "engagement.sans_suite", priority: "important", reason_template: "Engagement pris et resté sans suite", action: "notify", source: "system", enabled: true, last_fired: null, rule_text: null },
        { id: "sys.dossier_qui_deborde", type: "context", condition: "fichiers.dossier_encombre", priority: "info", reason_template: "Dossier qui déborde, à ranger", action: "notify", source: "system", enabled: true, last_fired: null, rule_text: null },
        { id: "sys.anniversaire_proche", type: "context", condition: "personne.anniversaire", priority: "info", reason_template: "Anniversaire d'un proche dans quelques jours", action: "notify", source: "system", enabled: true, last_fired: null, rule_text: null },
      ];
    // La toile, la chronologie et les habitudes (prévisualisation navigateur).
    case "memory_graph":
      return {
        stats: { relations: 386, noeuds: 74, contacts: 51, par_type: [] },
        correspondants: [
          { kind: "contact", id: "julie@exemple.fr", label: "Julie Martin", echanges: 48, last_seen: Date.now() / 1000 - 3600 },
          { kind: "contact", id: "marc@exemple.fr", label: "Marc Dubois", echanges: 22, last_seen: Date.now() / 1000 - 86400 },
        ],
        identites: [{ address: "paul@moi.fr", observations: 380, presence_pct: 94, confirmed: true }],
        identites_retenues: ["paul@moi.fr"],
      };
    case "memory_relations":
      return {
        trouve: true,
        noeud: { kind: "contact", id: "julie@exemple.fr", label: "Julie Martin" },
        documents_lies: [{ relation: "auteur_de", kind: "item", id: "i1", label: "Devis toiture", observations: 3, last_seen: Date.now() / 1000 - 7200 }],
        gens_en_commun: [{ relation: "co_destinataire", kind: "contact", id: "marc@exemple.fr", label: "Marc Dubois", observations: 6, last_seen: Date.now() / 1000 - 86400 }],
        rendez_vous: [],
        echanges_observes: 48,
      };
    case "memory_timeline":
      return {
        total: 3,
        jours: [
          {
            jour: "mardi 18 août 2026",
            entrees: [
              { at: Date.now() / 1000 - 3600, heure: "14h20", kind: "mail_recu", title: "Devis toiture", detail: "de Julie Martin", source_ref: "mail:1" },
              { at: Date.now() / 1000 - 7200, heure: "11h05", kind: "action", title: "Envoyer un mail à Marc", detail: "fait par Syn", source_ref: "a1" },
              { at: Date.now() / 1000 - 9000, heure: "09h30", kind: "rendez_vous", title: "Point chantier", detail: "Visio", source_ref: "e1" },
            ],
          },
        ],
      };
    case "habits_list":
      return [
        { id: "h1", topic: "mail.compte", subject: "", value: "Gmail", observations: 12, last_seen: Date.now() / 1000, status: "confirmed", evidence: "compte utilisé pour tes derniers envois", phrase: "Tu envoies tes mails depuis Gmail." },
        { id: "h2", topic: "mail.cloture", subject: "", value: "Bien à toi,", observations: 5, last_seen: Date.now() / 1000, status: "observed", evidence: "façon dont tu termines tes messages", phrase: "Tu termines tes messages par « Bien à toi, »." },
      ];
    case "memory_set_identity":
    case "habits_decide":
      return null;
    case "memory_rebuild":
      return { elements_relus: 420, habitudes: 6 };
    case "connector_status":
      return [
        { id: "files", type: "files", status: "connected", scopes: null, last_sync: null, detail: null },
        { id: "apple", type: "apple", status: "connected", scopes: null, last_sync: null, detail: "Accès local macOS (Mail, Contacts) sous permissions OS — pas un connecteur cloud." },
        { id: "google", type: "google", status: "needs_configuration", scopes: null, last_sync: null, detail: "OAuth à configurer : vérification d'app du fournisseur requise." },
        { id: "microsoft", type: "microsoft", status: "needs_configuration", scopes: null, last_sync: null, detail: "OAuth à configurer." },
        { id: "slack", type: "slack", status: "needs_configuration", scopes: null, last_sync: null, detail: "OAuth à configurer." },
        { id: "github", type: "github", status: "needs_configuration", scopes: null, last_sync: null, detail: "OAuth à configurer." },
        { id: "system", type: "system", status: "connected", scopes: null, last_sync: null, detail: null },
        { id: "screen", type: "screen", status: "disconnected", scopes: null, last_sync: null, detail: null },
      ];
    case "files_index_status":
      return {
        running: false,
        done: 1240,
        total: 1240,
        current: null,
        items_count: 1240,
        pending_embeddings: 0,
        sensitive_skipped: 4,
        unreadable_files: 2,
        folders: [
          { path: "/Users/paul/Documents", last_indexed: Date.now() / 1000 - 3600 },
          { path: "/Users/paul/Desktop", last_indexed: Date.now() / 1000 - 7200 },
        ],
      };
    case "knowledge_stats":
      return { by_type: [{ source: "files", type: "document", count: 1180 }, { source: "mail", type: "email", count: 420 }], people: 5, embeddings: 6231, facts: 12 };
    case "list_knowledge":
      return [
        { id: "k1", source: "files", source_ref: "/Users/paul/Documents/Rapport_IAM.docx", type: "document", title: "Rapport_IAM.docx", path: null, size: 128000, mtime: 0, ingested_at: Date.now() / 1000 },
        { id: "k2", source: "mail", source_ref: "mail:1", type: "email", title: "Réservation billets Paris → Lyon", path: null, size: null, mtime: 0, ingested_at: Date.now() / 1000 - 4000 },
      ];
    case "system_snapshot":
      return {
        snapshot: {
          os: "macOS 26.6", cpu_pct: 15.2, cpu_count: 10,
          mem_total_gb: 32, mem_used_gb: 18.4,
          disks: [{ mount: "/", total_gb: 994, free_gb: 312 }],
          temps: [{ label: "CPU", celsius: 47.5 }],
          battery: { pct: 82, charging: true },
          top_processes: [{ name: "Syn", cpu_pct: 3.1, mem_mb: 220 }, { name: "Safari", cpu_pct: 8.4, mem_mb: 812 }],
          uptime_secs: 86400,
        },
        explanation: "Rien d'anormal : charge CPU, mémoire, disque et température sont dans les valeurs normales.",
      };
    case "llm_status":
      return { available: true, runtime: "ollama", chat_model_ready: true, embed_model_ready: true, installed_models: ["llama3.1:latest", "nomic-embed-text"], detail: null };
    case "hardware_info":
      return { tier: "standard", total_ram_gb: 32, cpu_arch: "aarch64", cpu_count: 10, chat_model: "llama3.1:latest", embed_model: "nomic-embed-text" };
    case "storage_stats":
      return { db_bytes: 182_000_000, items: 1600, embeddings: 6231, data_dir: "/Users/paul/Library/Application Support/app.syn.desktop" };
    case "get_daily_wrap":
      return { greeting: "Bonsoir Monsieur,", done_tasks: [], pending_tasks: [], open_commitments: [], actions_executed_today: 2, generated_at: 0 };
    case "query":
      return {
        text: "Votre devis se trouve dans Documents/Projets. Je l'ai retrouvé grâce à son contenu [source:1].",
        sources: [{ item_id: "k1", source: "files", source_ref: "/Users/paul/Documents/Projets/Devis_X.pdf", title: "Devis_X.pdf", path: null, snippet: "", score: 0.92 }],
        pending_actions: [],
        session_id: "demo",
        degraded: false,
      };
    case "calendar_today":
      return [];
    default:
      return null;
  }
}
