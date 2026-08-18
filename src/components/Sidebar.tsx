// Sidebar (maquette App desktop) : Accueil … Déconnexion.
import { For, Show, type JSX } from "solid-js";
import { Icon } from "./Icon";
import { Logo } from "./Logo";
import {
  page,
  setPage,
  setSettingsOpen,
  setScreen,
  sidebarCollapsed,
  setSidebarCollapsed,
  type PageId,
} from "../lib/state";
import { ipc } from "../lib/ipc";
import { runningSessions, unreadSessions } from "../lib/conversations";

const MAIN_ITEMS: { id: PageId; label: string; icon: string }[] = [
  { id: "accueil", label: "Accueil", icon: "home" },
  { id: "conversations", label: "Conversations", icon: "messages-square" },
  { id: "apprendre", label: "Apprendre à Syn", icon: "book-open" },
  { id: "connaissances", label: "Connaissances", icon: "library-big" },
  { id: "connecteurs", label: "Connecteurs", icon: "workflow" },
  { id: "appareil", label: "Mon appareil", icon: "laptop" },
  { id: "archives", label: "Activité", icon: "square-activity" },
  { id: "programmations", label: "Mes programmations", icon: "clock" },
];

const MODE_ITEMS: { id: PageId; label: string; icon: string }[] = [
  { id: "travail", label: "Mode travail", icon: "briefcase" },
  { id: "economie", label: "Mode économie", icon: "Leaf--Streamline-Lucide" },
];

export function Sidebar(): JSX.Element {
  const logout = async () => {
    await ipc.lock();
    setScreen("locked");
  };
  return (
    <nav class="sidebar" classList={{ collapsed: sidebarCollapsed() }}>
      <div class="sidebar-head">
        <Logo size={20} />
        <button
          class="topbar-btn"
          title="Replier la barre latérale"
          onClick={() => setSidebarCollapsed(true)}
        >
          <Icon name="panel-left" size={16} />
        </button>
      </div>

      <For each={MAIN_ITEMS}>
        {(item) => (
          <button
            class="side-item"
            classList={{ active: page() === item.id }}
            onClick={() => setPage(item.id)}
          >
            <Icon name={item.icon} size={15} />
            {item.label}
            {/* Depuis une autre page, c'est le seul signe que Syn travaille
                encore — puis qu'une réponse attend d'être lue. */}
            <Show when={item.id === "conversations" && runningSessions().length > 0}>
              <span class="convo-working" role="status" aria-label="Syn travaille sur une conversation" />
            </Show>
            <Show
              when={
                item.id === "conversations" &&
                runningSessions().length === 0 &&
                unreadSessions().length > 0
              }
            >
              <span class="convo-unread" role="status" aria-label="Réponse non lue" />
            </Show>
          </button>
        )}
      </For>

      <div class="side-sep" />

      <For each={MODE_ITEMS}>
        {(item) => (
          <button
            class="side-item"
            classList={{ active: page() === item.id }}
            onClick={() => setPage(item.id)}
          >
            <Icon name={item.icon} size={15} />
            {item.label}
          </button>
        )}
      </For>

      <div class="side-sep" />

      <button class="side-item" onClick={() => setSettingsOpen(true)}>
        <Icon name="settings" size={15} />
        Réglages
      </button>
      <button class="side-item" onClick={logout}>
        <Icon name="log-out" size={15} />
        Déconnexion
      </button>
    </nav>
  );
}
