/* Racine de l'app : loading → onboarding | verrouillé | app. */
import { render } from "solid-js/web";
import { createEffect, createSignal, onMount, Show, type JSX } from "solid-js";
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
import { ipc } from "./lib/ipc";
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

function AlertsPopover(): JSX.Element {
  const [open, setOpen] = createSignal(false);
  return (
    <>
      <button class="topbar-btn" title="Notifications de Syn" onClick={() => setOpen(!open())}>
        <Icon name={alerts().length > 0 ? "bell-dot" : "bell"} size={17} />
        <Show when={alerts().length > 0}>
          <span class="badge-dot" />
        </Show>
      </button>
      <Show when={open()}>
        <div
          style={{
            position: "absolute",
            top: "34px",
            right: "0",
            width: "340px",
            "max-height": "420px",
            "overflow-y": "auto",
            background: "var(--bg-card)",
            "border-radius": "12px",
            "box-shadow": "var(--shadow-modal), inset 0 0 0 1px var(--border-subtle)",
            padding: "12px",
            "z-index": "50",
          }}
        >
          <Show when={alerts().length === 0}>
            <div class="muted" style={{ "text-align": "center", padding: "14px" }}>
              Rien à signaler — Syn ne parle que quand il a une raison.
            </div>
          </Show>
          {alerts().map((a) => (
            <div style={{ padding: "8px 6px", "border-bottom": "1px solid var(--border-subtle)" }}>
              <div style={{ "font-weight": "500", "margin-bottom": "3px" }}>{a.body}</div>
              <div class="sub muted">Pourquoi : {a.reason}</div>
            </div>
          ))}
          <Show when={alerts().length > 0}>
            <button class="link-btn" style={{ padding: "8px 6px" }} onClick={clearAlerts}>
              Tout effacer
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
