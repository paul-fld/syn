// Modes travail & économie — parties consultatives [V1] ; coercitif = [V2].
import { Show, createResource, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { Toggle, SettingRow } from "../components/Toggle";
import { ipc } from "../lib/ipc";
import { settings, refreshSettings } from "../lib/state";

export function ModeTravail(): JSX.Element {
  const patch = async (p: Record<string, unknown>) => {
    await ipc.setSettings(p);
    refreshSettings();
  };
  return (
    <div class="page">
      <div class="page-title">Mode travail</div>
      <div class="page-sub">
        Pendant le focus, Syn filtre ses propres surfaçages : seul l'urgent passe et les autres
        suggestions ne sont pas affichées. Le blocage forcé d'applications ou de sites relève des privilèges de l'OS et
        arrivera dans une version ultérieure.
      </div>
      <div class="card">
        <SettingRow
          label="Activer le mode travail"
          desc="Masque les notifications non urgentes de Syn pendant le focus."
        >
          <Toggle checked={settings()?.work_mode ?? false} onChange={(v) => patch({ work_mode: v })} />
        </SettingRow>
      </div>
      <Show when={settings()?.work_mode}>
        <div class="chip">
          <Icon name="briefcase" size={13} />
          Mode travail actif — seul l'urgent te parviendra.
        </div>
      </Show>
    </div>
  );
}

export function ModeEconomie(): JSX.Element {
  const [snap] = createResource(() => ipc.systemSnapshot());
  const patch = async (p: Record<string, unknown>) => {
    await ipc.setSettings(p);
    refreshSettings();
  };
  const battery = () => snap()?.snapshot?.battery;
  return (
    <div class="page">
      <div class="page-title">Mode économie</div>
      <div class="page-sub">
        Sur batterie faible, Syn se limite à l'essentiel : indexation en pause, proactivité
        réduite. Agir sur les réglages d'énergie de l'OS dépend des APIs de chaque plateforme.
      </div>
      <Show when={battery()}>
        <div class="chip" style={{ "margin-bottom": "14px" }}>
          <Icon name="leaf" size={13} />
          Batterie : {battery().pct} % · {battery().charging ? "sur secteur" : "sur batterie"}
        </div>
      </Show>
      <div class="card">
        <SettingRow
          label="Activer le mode économie"
          desc="Met l'indexation en pause et réduit l'activité de fond de Syn."
        >
          <Toggle checked={settings()?.eco_mode ?? false} onChange={(v) => patch({ eco_mode: v })} />
        </SettingRow>
        <SettingRow
          label="Pause de l'indexation seule"
          desc="Suspend uniquement l'ingestion de fichiers, sans toucher au reste."
        >
          <Toggle
            checked={settings()?.indexing_paused ?? false}
            onChange={(v) => patch({ indexing_paused: v })}
          />
        </SettingRow>
      </div>
    </div>
  );
}
