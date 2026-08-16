/* Racine de l'app : loading → onboarding | verrouillé | app. */
import { render } from "solid-js/web";
import { createEffect, createSignal, For, onMount, Show, type JSX } from "solid-js";
import "./styles/global.css";
import { Icon } from "./components/Icon";
import { SynGlyph } from "./components/Logo";
import { Sidebar } from "./components/Sidebar";
import { SettingsModal } from "./settings/SettingsModal";
import { Onboarding } from "./onboarding/Onboarding";
import { Accueil } from "./pages/Accueil";
import { Conversations } from "./pages/Conversations";
import { Apprendre } from "./pages/Apprendre";
import { Connaissances } from "./pages/Connaissances";
import { Connecteurs } from "./pages/Connecteurs";
import { MonAppareil } from "./pages/MonAppareil";
import { Archives } from "./pages/Archives";
import { Programmations } from "./pages/Programmations";
import { ModeTravail, ModeEconomie } from "./pages/Modes";
import { ipc, type SynNotification } from "./lib/ipc";
import { label } from "./lib/voice";
import {
  screen,
  page,
  setPage,
  refreshStatus,
  refreshPending,
  wireEvents,
  status,
  settings,
  sidebarCollapsed,
  setSidebarCollapsed,
  alerts,
  clearAlerts,
  refreshAlerts,
  setSettingsOpen,
  setSettingsTab,
  fmtDate,
  loadError,
} from "./lib/state";

function Locked(): JSX.Element {
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [recoveryMode, setRecoveryMode] = createSignal(false);
  const [phrase, setPhrase] = createSignal("");

  const tryUnlock = async () => {
    setError(null);
    try {
      if (recoveryMode()) await ipc.unlockWithRecovery(phrase());
      else await ipc.unlock(password());
      await refreshStatus();
      refreshPending();
    } catch (e: any) {
      setError(e?.message ?? String(e));
    }
  };

  const tryKeychain = async () => {
    try {
      await ipc.unlockWithKeychain();
      await refreshStatus();
    } catch (e: any) {
      setError(e?.message ?? String(e));
    }
  };

  return (
    <div class="lock-shell">
      <SynGlyph size={54} />
      <div style={{ "font-size": "17px", "font-weight": "500" }}>
        {label("lock.hint", settings()?.voice)}
      </div>
      <Show
        when={!recoveryMode()}
        fallback={
          <input
            class="text-input"
            style={{ width: "360px", "text-align": "center" }}
            placeholder="Phrase de récupération (12 mots)"
            value={phrase()}
            onInput={(e) => setPhrase(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && tryUnlock()}
          />
        }
      >
        <input
          class="text-input"
          style={{ width: "280px", "text-align": "center" }}
          type="password"
          placeholder="Mot de passe maître"
          value={password()}
          onInput={(e) => setPassword(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && tryUnlock()}
          ref={(el) => setTimeout(() => el.focus(), 80)}
        />
      </Show>
      <Show when={error()}>
        <div style={{ color: "var(--danger)", "font-size": "12.5px", "max-width": "380px", "text-align": "center" }}>
          {error()}
        </div>
      </Show>
      <div style={{ display: "flex", gap: "10px" }}>
        <button class="btn primary" onClick={tryUnlock}>
          Déverrouiller
        </button>
        <Show when={status()?.keychain}>
          <button class="btn" onClick={tryKeychain}>
            <Icon name="fingerprint" size={14} /> Trousseau
          </button>
        </Show>
      </div>
      <button class="link-btn" onClick={() => setRecoveryMode(!recoveryMode())}>
        {recoveryMode() ? "← Mot de passe" : "Mot de passe oublié ? Phrase de récupération"}
      </button>
    </div>
  );
}

const NOTIFICATION_SEEN_KEY = "syn.notifications.seen";

function notificationTitle(notification: SynNotification): string {
  switch (notification.kind) {
    case "brief": return "Résumé du jour";
    case "daily_wrap": return "Bilan du jour";
    case "event": return "Événement à venir";
    case "commitment": return "Échéance à venir";
    case "rule": return "Règle déclenchée";
    case "system": return notification.reason || "État de l'appareil";
    default: return notification.reason || "Notification de Syn";
  }
}

function notificationBody(notification: SynNotification): string {
  if (notification.kind === "brief") {
    return "Ton agenda, tes tâches et tes rappels sont disponibles sur l'accueil.";
  }
  return notification.body || notification.reason;
}

function notificationDestination(notification: SynNotification): { page: "accueil" | "appareil" | "programmations" | "archives"; label: string } {
  if (notification.kind === "system") return { page: "appareil", label: "Voir l'appareil" };
  if (notification.kind === "rule") return { page: "programmations", label: "Voir la règle" };
  if (notification.kind === "daily_wrap") return { page: "archives", label: "Voir dans Activité" };
  return { page: "accueil", label: notification.kind === "brief" ? "Voir le résumé" : "Voir l'accueil" };
}

function storedSeenNotifications(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(NOTIFICATION_SEEN_KEY) ?? "[]"));
  } catch {
    return new Set();
  }
}

function AlertsPopover(): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const [seen, setSeen] = createSignal(storedSeenNotifications());
  const unreadCount = () => alerts().filter((alert) => !seen().has(alert.id)).length;

  createEffect(() => {
    if (!open()) return;
    const next = new Set(seen());
    alerts().forEach((alert) => next.add(alert.id));
    setSeen(next);
    localStorage.setItem(NOTIFICATION_SEEN_KEY, JSON.stringify([...next].slice(-500)));
  });

  const dismiss = async (id: string) => {
    await ipc.dismissSurfacing(id);
    await refreshAlerts();
  };

  return (
    <>
      <button class="topbar-btn notification-button" title="Notifications" onClick={() => setOpen(!open())}>
        <Icon name="bell" size={17} />
        <Show when={unreadCount() > 0}>
          <span class="badge-dot" />
        </Show>
      </button>
      <Show when={open()}>
        <div class="notifications-popover">
          <div class="notifications-header">
            <b>Notifications</b>
            <button
              title="Régler les notifications"
              onClick={() => {
                setOpen(false);
                setSettingsTab("notifications");
                setSettingsOpen(true);
              }}
            >
              <Icon name="settings" size={14} />
            </button>
          </div>
          <Show when={alerts().length === 0}>
            <div class="notifications-empty">
              Aucune notification.
            </div>
          </Show>
          <For each={alerts()}>
            {(notification) => {
              const destination = notificationDestination(notification);
              return (
                <div class="notification-item">
                  <button
                    class="notification-content"
                    onClick={() => {
                      setPage(destination.page);
                      setOpen(false);
                    }}
                  >
                    <span class={`notification-icon ${notification.priority}`}>
                      <Icon name={notification.kind === "system" ? "gauge" : notification.kind === "event" ? "calendar" : notification.kind === "commitment" ? "flag" : notification.kind === "rule" ? "hash" : "bell"} size={14} />
                    </span>
                    <span class="notification-copy">
                      <span class="notification-title">{notificationTitle(notification)}</span>
                      <span class="notification-body">{notificationBody(notification)}</span>
                      <span class="notification-meta">
                        {fmtDate(notification.surfaced_at)} · {destination.label}
                      </span>
                    </span>
                  </button>
                  <button class="notification-dismiss" title="Supprimer" onClick={() => dismiss(notification.id)}>
                    <Icon name="x" size={12} />
                  </button>
                </div>
              );
            }}
          </For>
          <Show when={alerts().length > 0}>
            <button class="notifications-clear" onClick={async () => { await clearAlerts(); setOpen(false); }}>
              Effacer toutes les notifications
            </button>
          </Show>
        </div>
      </Show>
    </>
  );
}

function AppShell(): JSX.Element {
  return (
    <div class="app-shell">
      <div class="titlebar" />
      <div class="app-body">
        <Sidebar />
        <main class="content">
          <Show when={sidebarCollapsed()}>
            <button class="topbar-btn sidebar-reopen" title="Afficher la barre latérale" onClick={() => setSidebarCollapsed(false)}>
              <Icon name="panel-left" size={16} />
            </button>
          </Show>
          <div class="content-topbar" style={{ top: "10px", position: "absolute" }}>
            <div style={{ position: "relative", display: "flex", gap: "12px" }}>
              <AlertsPopover />
              <button class="topbar-btn" title={status()?.email ?? "Profil"} onClick={() => {
                setSettingsTab("compte");
                setSettingsOpen(true);
              }}>
                <Icon name="circle-user-round" size={18} />
              </button>
            </div>
          </div>

          <Show when={page() === "accueil"}>
            <Accueil />
          </Show>
          <Show when={page() === "conversations"}>
            <Conversations />
          </Show>
          <Show when={page() === "apprendre"}>
            <Apprendre />
          </Show>
          <Show when={page() === "connaissances"}>
            <Connaissances />
          </Show>
          <Show when={page() === "connecteurs"}>
            <Connecteurs />
          </Show>
          <Show when={page() === "appareil"}>
            <MonAppareil />
          </Show>
          <Show when={page() === "archives"}>
            <Archives />
          </Show>
          <Show when={page() === "programmations"}>
            <Programmations />
          </Show>
          <Show when={page() === "travail"}>
            <ModeTravail />
          </Show>
          <Show when={page() === "economie"}>
            <ModeEconomie />
          </Show>
        </main>
      </div>
      <SettingsModal />
    </div>
  );
}

function App(): JSX.Element {
  createEffect(() => {
    const st = settings();
    document.documentElement.classList.toggle("large-text", st?.large_text ?? false);
    document.documentElement.classList.toggle("reduce-motion", st?.reduce_motion ?? false);
  });
  onMount(async () => {
    await wireEvents();
    await refreshStatus();
    refreshPending();
    refreshAlerts();
  });
  return (
    <>
      <Show when={screen() === "loading"}>
        <div class="lock-shell">
          <SynGlyph size={54} />
          <Show when={loadError()}>
            <div class="empty-note" style={{ "margin-top": "14px", "max-width": "360px", "text-align": "center" }}>
              Syn n'a pas pu démarrer : {loadError()}
            </div>
            <button class="btn" style={{ "margin-top": "10px" }} onClick={() => refreshStatus()}>
              Réessayer
            </button>
          </Show>
        </div>
      </Show>
      <Show when={screen() === "onboarding"}>
        <Onboarding />
      </Show>
      <Show when={screen() === "locked"}>
        <Locked />
      </Show>
      <Show when={screen() === "app"}>
        <AppShell />
      </Show>
    </>
  );
}

render(() => <App />, document.getElementById("root")!);

// Retour à l'Accueil quand la fenêtre est réaffichée depuis le tray.
void setPage;
