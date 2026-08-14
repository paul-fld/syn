// Réglages (maquette dédiée) : modale, colonne d'onglets à gauche, X en haut à droite.
import { For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { settingsOpen, setSettingsOpen, settingsTab, setSettingsTab } from "../lib/state";
import { TabGeneral, TabNotifications, TabCompte, TabPersonnalisation, TabAccessibilite } from "./TabsBasic";
import { TabRegles } from "./TabRegles";
import { TabDonnees, TabStockage, TabEnfant, TabSecurite, TabConfidentialite, TabAide } from "./TabsData";

const TABS: { id: string; label: string; icon: string | null; v2?: boolean }[] = [
  { id: "general", label: "Général", icon: "settings" },
  { id: "notifications", label: "Notifications", icon: "bell" },
  { id: "regles", label: "Règles", icon: "hash" },
  { id: "compte", label: "Compte", icon: "circle-user-round" },
  { id: "personnalisation", label: "Personnalisation", icon: "palette" },
  { id: "accessibilite", label: "Accessibilité", icon: "person-standing" },
  { id: "donnees", label: "Données", icon: "database" },
  { id: "stockage", label: "Stockage", icon: "hard-drive" },
  { id: "enfant", label: "Appareil de mon enfant", icon: "baby", v2: true },
  { id: "securite", label: "Sécurité", icon: "key-round" },
  { id: "confidentialite", label: "Confidentialité", icon: "shield" },
  { id: "aide", label: "Aide", icon: "life-buoy" },
];

export function SettingsModal(): JSX.Element {
  return (
    <Show when={settingsOpen()}>
      <div class="modal-backdrop" onClick={(e) => e.target === e.currentTarget && setSettingsOpen(false)}>
        <div class="settings-modal fade-in">
          <button class="settings-close" onClick={() => setSettingsOpen(false)}>
            <Icon name="x" size={18} />
          </button>

          <div class="settings-side">
            <For each={TABS}>
              {(t) => (
                <button
                  class="settings-tab"
                  classList={{ active: settingsTab() === t.id }}
                  onClick={() => setSettingsTab(t.id)}
                >
                  <Show
                    when={t.icon}
                    fallback={
                      <span
                        class="icon"
                        style={{
                          width: "17px",
                          "text-align": "center",
                          "font-size": "16px",
                          "font-weight": "600",
                          color: "var(--text-secondary)",
                        }}
                      >
                        #
                      </span>
                    }
                  >
                    <Icon name={t.icon!} size={17} />
                  </Show>
                  {t.label}
                  <Show when={t.v2}>
                    <span class="v2-badge">bientôt</span>
                  </Show>
                </button>
              )}
            </For>
          </div>

          <div class="settings-body">
            <Show when={settingsTab() === "general"}>
              <TabGeneral />
            </Show>
            <Show when={settingsTab() === "regles"}>
              <TabRegles />
            </Show>
            <Show when={settingsTab() === "notifications"}>
              <TabNotifications />
            </Show>
            <Show when={settingsTab() === "compte"}>
              <TabCompte />
            </Show>
            <Show when={settingsTab() === "personnalisation"}>
              <TabPersonnalisation />
            </Show>
            <Show when={settingsTab() === "accessibilite"}>
              <TabAccessibilite />
            </Show>
            <Show when={settingsTab() === "donnees"}>
              <TabDonnees />
            </Show>
            <Show when={settingsTab() === "stockage"}>
              <TabStockage />
            </Show>
            <Show when={settingsTab() === "enfant"}>
              <TabEnfant />
            </Show>
            <Show when={settingsTab() === "securite"}>
              <TabSecurite />
            </Show>
            <Show when={settingsTab() === "confidentialite"}>
              <TabConfidentialite />
            </Show>
            <Show when={settingsTab() === "aide"}>
              <TabAide />
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
