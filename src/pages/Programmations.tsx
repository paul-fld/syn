// Mes programmations : déclencheurs actifs (briefs, gardien, règles de fond).
import { createResource, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { Toggle } from "../components/Toggle";
import { ipc } from "../lib/ipc";
import { fmtDate, settings, refreshSettings } from "../lib/state";

export function Programmations(): JSX.Element {
  const [triggers, { refetch }] = createResource(() => ipc.listTriggers());

  const patch = async (p: Record<string, unknown>) => {
    await ipc.setSettings(p);
    refreshSettings();
  };

  return (
    <div class="page">
      <div class="page-title">Mes programmations</div>
      <div class="page-sub">
        Ce que Syn fait de lui-même — toujours rare, toujours explicable, sous budget ({settings()?.rarity_budget ?? 5}
        {" "}surfaçages/jour max). Chaque ligne montre sa raison d'exister.
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="bell" size={15} /> Briefs quotidiens
        </div>
        <div class="row-line">
          <Icon name="alarm-clock" size={14} />
          <span class="grow">
            Brief de démarrage
            <span class="sub"> — au premier réveil du jour, après {settings()?.brief_floor_hour ?? 7}h</span>
          </span>
          <Toggle
            checked={settings()?.startup_brief_enabled ?? true}
            onChange={(v) => patch({ startup_brief_enabled: v })}
          />
        </div>
        <div class="row-line">
          <Icon name="bed-double" size={14} />
          <span class="grow">
            Débrief de fin de journée
            <span class="sub"> — vers {settings()?.daily_wrap_hour ?? 18}h : bouclé, glissé, promesses</span>
          </span>
          <Toggle
            checked={settings()?.daily_wrap_enabled ?? true}
            onChange={(v) => patch({ daily_wrap_enabled: v })}
          />
        </div>
        <div class="row-line">
          <Icon name="gauge" size={14} />
          <span class="grow">
            Gardien système
            <span class="sub"> — disque &lt; {settings()?.guardian_disk_pct}% libre, température &gt; {settings()?.guardian_temp_c}°C</span>
          </span>
          <span class="pill-status ok">Actif</span>
        </div>
      </div>

      <div class="section-label">Tâches de fond issues de tes règles</div>
      <Show when={(triggers() ?? []).filter((t: any) => t.source === "rule").length === 0}>
        <div class="empty-note">
          Aucune. Ajoute une règle comme « #Surveille régulièrement les performances de mon
          ordinateur » dans Réglages → Règles.
        </div>
      </Show>
      <For each={(triggers() ?? []).filter((t: any) => t.source === "rule")}>
        {(t: any) => (
          <div class="row-line">
            <Icon name="repeat" size={14} />
            <span class="grow">
              {t.rule_text ?? t.reason_template}
              <span class="sub">
                {" "}· {t.type} · {t.condition}
                <Show when={t.last_fired}> · dernier déclenchement {fmtDate(t.last_fired)}</Show>
              </span>
            </span>
            <Toggle checked={t.enabled} onChange={(v) => ipc.triggerToggle(t.id, v).then(() => refetch())} />
          </div>
        )}
      </For>
    </div>
  );
}
