// Store global de l'app (SolidJS) : statut, réglages, événements backend.
import { createSignal } from "solid-js";
import { ipc, on, type AppStatus, type Settings, type PendingAction } from "./ipc";

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
export const [alerts, setAlerts] = createSignal<any[]>([]);
export const [briefVersion, setBriefVersion] = createSignal(0);
export const [sessionsVersion, setSessionsVersion] = createSignal(0);
// Requête envoyée depuis la barre d'interaction → ouvre Conversations.
export const [barQuery, setBarQuery] = createSignal<string | null>(null);

export async function refreshStatus() {
  const s = await ipc.appStatus();
  setStatus(s);
  if (!s.initialized) setScreen("onboarding");
  else if (!s.unlocked) setScreen("locked");
  else {
    const st = await ipc.getSettings();
    setSettings(st);
    if (!st.onboarding_done) setScreen("onboarding");
    else setScreen("app");
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
    setAlerts((a) => [{ ...p, ts: Date.now() }, ...a].slice(0, 20));
  });
  await on("bar_query", (text) => {
    if (typeof text === "string" && text.trim()) {
      setBarQuery(text);
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
