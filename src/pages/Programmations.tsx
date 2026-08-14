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
        Gère les résumés quotidiens, les alertes de l'appareil et tes règles automatiques.
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="bell" size={15} /> Résumés quotidiens
        </div>
        <div class="row-line">
          <Icon name="alarm-clock" size={14} />
          <span class="grow">
            Résumé du jour
            <span class="sub"> après {settings()?.brief_floor_hour ?? 7}h</span>
          </span>
          <Toggle
            checked={settings()?.startup_brief_enabled ?? true}
            onChange={(v) => patch({ startup_brief_enabled: v })}
          />
        </div>
        <div class="row-line">
          <Icon name="bed-double" size={14} />
          <span class="grow">
            Bilan du soir
            <span class="sub"> à partir de {settings()?.daily_wrap_hour ?? 18}h</span>
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
            <span class="sub"> stockage et température</span>
          </span>
          <span class="pill-status ok">Actif</span>
        </div>
      </div>

      <div class="section-label">Règles automatiques</div>
      <Show when={(triggers() ?? []).filter((t: any) => t.source === "rule").length === 0}>
        <div class="empty-note">
          Aucune règle automatique.
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
