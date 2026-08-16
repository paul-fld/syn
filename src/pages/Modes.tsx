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
        Réduit les interruptions pendant une session de concentration.
      </div>
      <div class="card">
        <SettingRow
          label="Activer le mode travail"
          desc="Filtre les notifications et les suggestions de Syn."
        >
          <Toggle checked={settings()?.work_mode ?? false} onChange={(v) => patch({ work_mode: v })} />
        </SettingRow>
        <SettingRow label="Notifications autorisées" desc="Les alertes urgentes restent toujours visibles.">
          <select
            class="select"
            value={settings()?.work_notification_policy ?? "urgent"}
            onChange={(event) => patch({ work_notification_policy: event.currentTarget.value })}
          >
            <option value="urgent">Urgentes uniquement</option>
            <option value="relevant">Urgentes, agenda et échéances</option>
          </select>
        </SettingRow>
      </div>
      <Show when={settings()?.work_mode}>
        <div class="chip">
          <Icon name="briefcase" size={13} />
          Mode travail actif
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
  const batteryIcon = () => {
    const pct = Number(battery()?.pct ?? 100);
    return pct < 30 ? "battery-low" : pct < 70 ? "battery-medium" : "battery-full";
  };
  return (
    <div class="page">
      <div class="page-title">Mode économie</div>
      <div class="page-sub">
        Réduit l'activité de Syn pour préserver la batterie.
      </div>
      <Show when={battery()}>
        <div class="chip" style={{ "margin-bottom": "14px" }}>
          <Icon name={batteryIcon()} size={13} />
          Batterie : {battery().pct} % · {battery().charging ? "sur secteur" : "sur batterie"}
        </div>
      </Show>
      <div class="card">
        <SettingRow
          label="Activer le mode économie"
          desc="Réduit la proactivité et les tâches non essentielles ; la surveillance des fichiers reste active."
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
