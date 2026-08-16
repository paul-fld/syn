// Store global de l'app (SolidJS) : statut, réglages, événements backend.
import { createSignal } from "solid-js";
import { ipc, on, type AppStatus, type Settings, type PendingAction, type ScreenContext, type SynNotification } from "./ipc";

export type Screen = "loading" | "onboarding" | "locked" | "app";
export type PageId =
  | "accueil"
  | "conversations"
  | "apprendre"
  | "connaissances"
  | "connecteurs"
  | "appareil"
  | "archives"
  | "programmations"
  | "travail"
  | "economie";

export const [screen, setScreen] = createSignal<Screen>("loading");
export const [page, setPage] = createSignal<PageId>("accueil");
export const [settings, setSettings] = createSignal<Settings | null>(null);
export const [status, setStatus] = createSignal<AppStatus | null>(null);
export const [settingsOpen, setSettingsOpen] = createSignal(false);
export const [settingsTab, setSettingsTab] = createSignal("general");
export const [sidebarCollapsed, setSidebarCollapsed] = createSignal(false);
export const [pendingActions, setPendingActions] = createSignal<PendingAction[]>([]);
export const [alerts, setAlerts] = createSignal<SynNotification[]>([]);
export const [briefVersion, setBriefVersion] = createSignal(0);
export const [sessionsVersion, setSessionsVersion] = createSignal(0);
// Requête envoyée depuis la barre d'interaction → ouvre Conversations.
export interface PendingQuery { text: string; screenContext?: ScreenContext | null }
export const [barQuery, setBarQuery] = createSignal<PendingQuery | null>(null);

export const [loadError, setLoadError] = createSignal<string | null>(null);

export async function refreshStatus() {
  // Sans garde-fou, un échec ici laissait l'app bloquée sur le glyphe de
  // chargement sans message ni retry (audit §3).
  try {
    const s = await ipc.appStatus();
    setStatus(s);
    setLoadError(null);
    if (!s.initialized) setScreen("onboarding");
    else if (!s.unlocked) setScreen("locked");
    else {
      const st = await ipc.getSettings();
      setSettings(st);
      if (!st.onboarding_done) setScreen("onboarding");
      else setScreen("app");
    }
  } catch (e: any) {
    setLoadError(e?.message ?? String(e));
  }
}

export async function refreshSettings() {
  try {
    setSettings(await ipc.getSettings());
  } catch {}
}

export async function refreshPending() {
  try {
    setPendingActions(await ipc.listPendingActions());
  } catch {
    setPendingActions([]);
  }
}

export async function refreshAlerts() {
  try {
    const rows = await ipc.listSurfacings(20);
    setAlerts(rows.filter((x: any) => !x.dismissed));
  } catch {
    setAlerts([]);
  }
}

export async function clearAlerts() {
  const current = alerts();
  await Promise.all(current.filter((a) => a.id).map((a) => ipc.dismissSurfacing(a.id).catch(() => {})));
  setAlerts([]);
}

let wired = false;
export async function wireEvents() {
  if (wired) return;
  wired = true;
  await on("voice_profile_changed", () => refreshSettings());
  await on("action_awaiting_confirmation", () => refreshPending());
  await on("action_resolved", () => refreshPending());
  await on("brief_ready", () => setBriefVersion((v) => v + 1));
  await on("proactive_alert", (p) => {
    const alert = p?.payload ?? p;
    if (!alert?.id) return;
    setAlerts((current) => [
      { ...alert, body: alert.body ?? null, surfaced_at: Math.floor(Date.now() / 1000), dismissed: false },
      ...current.filter((item) => item.id !== alert.id),
    ].slice(0, 20));
  });
  await on("bar_query", (text) => {
    if (typeof text === "string" && text.trim()) {
      setBarQuery({ text });
      setPage("conversations");
    }
  });
  await on("bar_conversation_updated", () => setSessionsVersion((v) => v + 1));
}

export function fmtDate(ts: number | null | undefined): string {
  if (!ts) return "";
  return new Date(ts * 1000).toLocaleString("fr-FR", {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function fmtBytes(n: number): string {
  if (n > 1e9) return (n / 1e9).toFixed(1) + " Go";
  if (n > 1e6) return (n / 1e6).toFixed(1) + " Mo";
  if (n > 1e3) return (n / 1e3).toFixed(0) + " Ko";
  return n + " o";
}
