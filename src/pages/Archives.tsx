// Activité : actions (+annulation), accès aux données et notifications proactives.
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ReorganizePlanView } from "../components/ActionCard";
import { ipc } from "../lib/ipc";
import { fmtDate } from "../lib/state";

const TABS = [
  { id: "actions", label: "Actions effectuées" },
  { id: "access", label: "Accès aux données" },
  { id: "proactive", label: "Notifications de Syn" },
];

const STATUS_FR: Record<string, string> = {
  executed: "exécutée",
  awaiting_confirmation: "en attente",
  rejected: "refusée",
  undone: "annulée",
  failed: "échouée",
};

const RISK_FR: Record<string, string> = {
  read: "Lecture seule",
  reversible_local: "Annulable sur cet appareil",
  reversible_external: "Action externe annulable",
  floor: "Confirmation obligatoire",
};

const NOTIFICATION_TITLE: Record<string, string> = {
  brief: "Résumé du jour",
  daily_wrap: "Bilan du jour",
  event: "Événement à venir",
  commitment: "Échéance à venir",
  rule: "Règle déclenchée",
};

export function Archives(): JSX.Element {
  const [tab, setTab] = createSignal("actions");
  const [actions, { refetch: refetchActions }] = createResource(() => ipc.listActions(null, 200));
  const [access] = createResource(() => ipc.accessLogList(200));
  const [surfacings] = createResource(() => ipc.listSurfacings(100));

  return (
    <div class="page">
      <div class="page-title">Activité</div>
      <div class="page-sub">
        Retrouve les actions de Syn, les accès à tes données et les notifications reçues.
      </div>

      <div style={{ display: "flex", gap: "8px", "margin-bottom": "16px" }}>
        <For each={TABS}>
          {(t) => (
            <button
              class="btn"
              style={{ background: tab() === t.id ? "var(--bg-selected)" : "transparent" }}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          )}
        </For>
      </div>

      <Show when={tab() === "actions"}>
        <Show when={(actions() ?? []).length === 0}>
          <div class="empty-note">Aucune action pour l'instant.</div>
        </Show>
        <For each={actions() ?? []}>
          {(a) => (
            <div class="archive-action">
              <div class="row-line">
              <Icon
                name={a.status === "executed" ? "circle-check-big" : a.status === "awaiting_confirmation" ? "hourglass" : a.status === "undone" ? "redo-2" : "x"}
                size={14}
              />
              <span class="grow" title={JSON.stringify(a.input)}>
                {a.preview || a.tool}
                <span class="sub">
                  {" "}· {a.tool} · {STATUS_FR[a.status] ?? a.status} · {fmtDate(a.created_at)}
                  <Show when={a.derived_from_untrusted}> · ⚠ dérivée de contenu non fiable</Show>
                </span>
              </span>
              <span class={`pill-status ${a.risk_class === "floor" ? "warn" : ""}`}>{RISK_FR[a.risk_class] ?? a.risk_class}</span>
                <Show when={a.status === "executed" && a.undoable}>
                <button
                  class="btn"
                  title="Annuler cette action"
                  onClick={async () => {
                    try {
                      await ipc.undoAction(a.id);
                    } catch (e: any) {
                      alert(e?.message ?? e);
                    }
                    refetchActions();
                  }}
                >
                  Annuler
                </button>
                </Show>
              </div>
              <Show when={a.tool === "files.apply_reorganize_plan"}>
                <ReorganizePlanView input={a.input} compact />
              </Show>
            </div>
          )}
        </For>
      </Show>

      <Show when={tab() === "access"}>
        <div class="empty-note activity-explanation">
          Syn enregistre les recherches, synchronisations et utilisations de données.
        </div>
        <Show when={(access() ?? []).length === 0}>
          <div class="empty-note">Aucun accès enregistré.</div>
        </Show>
        <For each={access() ?? []}>
          {(l: any) => (
            <div class="row-line">
              <Icon name="eye" size={13} />
              <span class="grow">
                <b>{l.connector}</b> · {l.operation}
                <Show when={l.item_ref}>
                  <span class="sub"> · {l.item_ref}</span>
                </Show>
              </span>
              <span class="sub">{fmtDate(l.created_at)}</span>
            </div>
          )}
        </For>
      </Show>

      <Show when={tab() === "proactive"}>
        <div class="empty-note activity-explanation">
          Historique des notifications affichées par Syn.
        </div>
        <Show when={(surfacings() ?? []).length === 0}>
          <div class="empty-note">Aucune notification enregistrée.</div>
        </Show>
        <For each={surfacings() ?? []}>
          {(s: any) => (
            <div class="row-line" style={{ "align-items": "flex-start" }}>
              <Icon name={s.kind === "system" ? "gauge" : s.kind === "brief" ? "bell" : "bell-dot"} size={14} />
              <span class="grow" style={{ "white-space": "normal" }}>
                <b>{NOTIFICATION_TITLE[s.kind] ?? s.reason}</b>
                <Show when={s.body}>
                  <div class="sub" style={{ "white-space": "normal" }}>
                    {s.kind === "brief"
                      ? "Ton agenda, tes tâches et tes rappels sont disponibles sur l'accueil."
                      : s.body}
                  </div>
                </Show>
              </span>
              <span class="sub">{fmtDate(s.surfaced_at)}</span>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
