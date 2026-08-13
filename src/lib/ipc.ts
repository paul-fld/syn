// Pont IPC typé vers le backend (contrat doc maître §28).
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { demoInvoke } from "./demo";

// Hors Tauri (prévisualisation navigateur) : mock avec les données des maquettes.
const HAS_TAURI = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const invoke: typeof tauriInvoke = HAS_TAURI
  ? tauriInvoke
  : ((cmd: string, args?: any) => demoInvoke(cmd, args)) as typeof tauriInvoke;

export interface AppStatus {
  initialized: boolean;
  unlocked: boolean;
  onboarding_done: boolean;
  email: string | null;
  keychain: boolean;
  keychain_available: boolean;
}

export interface VoiceProfile {
  formality: "tu" | "vous";
  address_form: string | null;
  extras: string[];
}

export interface Settings {
  autonomy: "prudent" | "assiste" | "autonome";
  startup_brief_enabled: boolean;
  brief_floor_hour: number;
  brief_sections: string[];
  daily_wrap_enabled: boolean;
  daily_wrap_hour: number;
  autostart: boolean;
  voice: VoiceProfile;
  theme: string;
  voice_input_enabled: boolean;
  voice_output_enabled: boolean;
  bar_shortcut: string;
  reduce_motion: boolean;
  large_text: boolean;
  cloud_escalation: boolean;
  sensitive_consent: boolean;
  rarity_budget: number;
  guardian_disk_pct: number;
  guardian_temp_c: number;
  work_mode: boolean;
  eco_mode: boolean;
  indexing_paused: boolean;
  tier: string;
  chat_model: string;
  embed_model: string;
  ollama_url: string;
  onboarding_done: boolean;
  last_brief_date: string;
  last_wrap_date: string;
}

export interface Retrieved {
  item_id: string;
  source: string;
  source_ref: string;
  title: string;
  path: string | null;
  snippet: string;
  score: number;
}

export interface PendingRef {
  action_id: string;
  tool: string;
  preview: string;
  risk_class: string;
}

export interface Answer {
  text: string;
  sources: Retrieved[];
  pending_actions: PendingRef[];
  session_id: string;
  degraded: boolean;
}

export interface BriefItem {
  icon: string;
  text: string;
  sub: string | null;
  source_ref: string | null;
  kind: string;
}

export interface BriefChip {
  icon: string;
  text: string;
  source_ref: string | null;
}

export interface Brief {
  greeting: string;
  items: BriefItem[];
  chips: BriefChip[];
  empty: boolean;
  generated_at: number;
}

export interface PendingAction {
  id: string;
  tool: string;
  input: unknown;
  risk_class: string;
  status: string;
  preview: string;
  result: string | null;
  created_at: number;
  derived_from_untrusted: boolean;
  session_id: string | null;
  undoable: boolean;
}

export interface AgentProgress {
  session_id: string;
  stage: string;
  title: string;
  detail: string | null;
  current: number;
  total: number;
  status: "running" | "waiting" | "done" | "error";
}

export interface ConnectorInfo {
  id: string;
  type: string;
  status: string;
  scopes: string | null;
  last_sync: number | null;
  detail: string | null;
}

export interface NativePermission {
  id: string;
  label: string;
  status: "granted" | "limited" | "denied" | "restricted" | "needs_permission" | "needs_selection" | "unavailable";
  detail: string;
  settings: string;
  operational: boolean;
}

export interface NativePermissions {
  platform: string;
  provider: string;
  services: NativePermission[];
}

export interface Rule {
  id: string;
  text: string;
  kind: string | null;
  status: string;
  priority: number;
  params: unknown;
  reason: string | null;
  created_at: number;
}

export interface RuleOutcome {
  status: string;
  kind: string | null;
  reason: string | null;
  id: string | null;
  conflict_with: string | null;
}

export interface IndexStatus {
  running: boolean;
  done: number;
  total: number;
  current: string | null;
  items_count: number;
  pending_embeddings: number;
  sensitive_skipped: number;
  folders: { path: string; last_indexed: number | null }[];
}

export interface HardwareProfile {
  tier: string;
  total_ram_gb: number;
  cpu_arch: string;
  cpu_count: number;
  chat_model: string;
  embed_model: string;
}

export interface LlmStatus {
  available: boolean;
  runtime: string;
  chat_model_ready: boolean;
  embed_model_ready: boolean;
  installed_models: string[];
  detail: string | null;
}

export const ipc = {
  // session & sécurité
  appStatus: () => invoke<AppStatus>("app_status"),
  setupMaster: (email: string | null, password: string) =>
    invoke<{ recovery_phrase: string }>("setup_master", { email, password }),
  unlock: (password: string) => invoke<void>("unlock", { password }),
  unlockWithKeychain: () => invoke<void>("unlock_with_keychain"),
  unlockWithRecovery: (phrase: string) => invoke<void>("unlock_with_recovery", { phrase }),
  lock: () => invoke<void>("lock"),
  changeMasterPassword: (current: string, newPassword: string) =>
    invoke<void>("change_master_password", { current, newPassword }),
  regenerateRecovery: (password: string) => invoke<string>("regenerate_recovery", { password }),
  setKeychain: (enabled: boolean) => invoke<void>("set_keychain", { enabled }),

  // conversation
  query: (sessionId: string | null, text: string) =>
    invoke<Answer>("query", { sessionId, text }),
  listSessions: () => invoke<any[]>("list_sessions"),
  getConversation: (sessionId: string) => invoke<any[]>("get_conversation", { sessionId }),

  // briefs
  getStartupBrief: () => invoke<Brief>("get_startup_brief"),
  getDailyWrap: () => invoke<any>("get_daily_wrap"),

  // actions
  listPendingActions: () => invoke<PendingAction[]>("list_pending_actions"),
  listActions: (status: string | null, limit?: number) =>
    invoke<PendingAction[]>("list_actions", { status, limit }),
  confirmAction: (actionId: string) => invoke<any>("confirm_action", { actionId }),
  rejectAction: (actionId: string) => invoke<void>("reject_action", { actionId }),
  undoAction: (actionId: string) => invoke<string>("undo_action", { actionId }),

  // connecteurs
  connectorStatus: () => invoke<ConnectorInfo[]>("connector_status"),
  connectorConnect: (id: string) => invoke<any>("connector_connect", { id }),
  connectorDisconnect: (id: string) => invoke<void>("connector_disconnect", { id }),
  nativePermissions: () => invoke<NativePermissions>("native_permissions"),
  requestNativePermission: (service: string) => invoke<{ service: string; status: string }>("request_native_permission", { service }),
  openNativeSettings: (section: string) => invoke<void>("open_native_settings", { section }),
  screenContext: () => invoke<any>("screen_context"),

  // réglages & modèle
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (patch: Partial<Settings> | Record<string, unknown>) =>
    invoke<Settings>("set_settings", { patch }),
  hardwareInfo: () => invoke<HardwareProfile>("hardware_info"),
  llmStatus: () => invoke<LlmStatus>("llm_status"),
  modelPull: (model: string) => invoke<void>("model_pull", { model }),

  // files
  filesAddFolder: (path: string) => invoke<void>("files_add_folder", { path }),
  filesRemoveFolder: (path: string) => invoke<void>("files_remove_folder", { path }),
  filesReindex: (path?: string) => invoke<void>("files_reindex", { path: path ?? null }),
  filesIndexStatus: () => invoke<IndexStatus>("files_index_status"),
  filesSearch: (queryText: string) => invoke<Retrieved[]>("files_search", { queryText }),
  searchMemory: (queryText: string) => invoke<Retrieved[]>("search_memory", { queryText }),

  // connaissances
  knowledgeStats: () => invoke<any>("knowledge_stats"),
  listKnowledge: (source: string | null, filter: string | null, limit?: number) =>
    invoke<any[]>("list_knowledge", { source, filter, limit }),
  forgetItem: (itemId: string) => invoke<void>("forget_item", { itemId }),

  // personnes
  getPersonContext: (name: string) => invoke<any>("get_person_context", { name }),
  peopleList: () => invoke<any[]>("people_list"),
  peopleOsPreview: () => invoke<any[]>("people_os_preview"),
  peopleAdd: (p: {
    name: string;
    relationship?: string;
    email?: string;
    phone?: string;
    birthday?: string;
  }) =>
    invoke<string>("people_add", {
      name: p.name,
      relationship: p.relationship ?? null,
      email: p.email ?? null,
      phone: p.phone ?? null,
      birthday: p.birthday ?? null,
    }),
  unknownsPending: () => invoke<any[]>("unknowns_pending"),
  unknownLabel: (unknownId: string, name: string, relationship?: string) =>
    invoke<void>("unknown_label", { unknownId, name, relationship: relationship ?? null }),
  unknownIgnore: (unknownId: string) => invoke<void>("unknown_ignore", { unknownId }),
  addFact: (text: string) => invoke<string>("add_fact", { text }),

  // règles
  rulesAdd: (text: string) => invoke<RuleOutcome>("rules_add", { text }),
  rulesEdit: (id: string, text: string) => invoke<RuleOutcome>("rules_edit", { id, text }),
  rulesDelete: (id: string) => invoke<void>("rules_delete", { id }),
  rulesList: () => invoke<Rule[]>("rules_list"),
  rulesSetPriority: (id: string, overId: string) =>
    invoke<void>("rules_set_priority", { id, overId }),

  // proactivité & système
  listSurfacings: (limit?: number) => invoke<any[]>("list_surfacings", { limit }),
  dismissSurfacing: (id: string) => invoke<void>("dismiss_surfacing", { id }),
  listTriggers: () => invoke<any[]>("list_triggers"),
  triggerToggle: (id: string, enabled: boolean) =>
    invoke<void>("trigger_toggle", { id, enabled }),
  systemSnapshot: () => invoke<any>("system_snapshot"),
  accessLogList: (limit?: number) => invoke<any[]>("access_log_list", { limit }),

  // calendrier & tâches
  calendarToday: () => invoke<any[]>("calendar_today"),
  tasksQuickAdd: (title: string, due?: string) =>
    invoke<string>("tasks_quick_add", { title, due: due ?? null }),

  // données & divers
  storageStats: () => invoke<any>("storage_stats"),
  dataDirPath: () => invoke<string>("data_dir_path"),
  purgeAllData: (password: string) => invoke<void>("purge_all_data", { password }),
  onboardingComplete: () => invoke<void>("onboarding_complete"),
  openSource: (sourceRef: string) => invoke<void>("open_source", { sourceRef }),
  showMainWindow: () => invoke<void>("show_main_window"),
  hideBar: () => invoke<void>("hide_bar"),
};

export function on(event: string, cb: (payload: any) => void): Promise<UnlistenFn> {
  if (!HAS_TAURI) return Promise.resolve(() => {});
  return listen(event, (e) => cb(e.payload));
}
