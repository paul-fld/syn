// Onglet Règles (maquette Réglages) : éditeur + « Vos règles » (tu/vous templaté).
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc, type Rule, type RuleOutcome } from "../lib/ipc";
import { label } from "../lib/voice";
import { settings, refreshSettings } from "../lib/state";

export function TabRegles(): JSX.Element {
  const [rules, { refetch }] = createResource(() => ipc.rulesList());
  const [draft, setDraft] = createSignal("");
  const [editing, setEditing] = createSignal<Rule | null>(null);
  const [feedback, setFeedback] = createSignal<RuleOutcome | null>(null);
  const [busy, setBusy] = createSignal(false);

  const submit = async () => {
    const text = draft().trim();
    if (!text || busy()) return;
    setBusy(true);
    setFeedback(null);
    try {
      const outcome = editing()
        ? await ipc.rulesEdit(editing()!.id, text)
        : await ipc.rulesAdd(text);
      setFeedback(outcome);
      if (outcome.status === "active") {
        setDraft("");
        setEditing(null);
      }
      refetch();
      refreshSettings(); // le profil de voix a pu changer → re-render tu/vous
    } finally {
      setBusy(false);
    }
  };

  const resolveConflict = async (keepNew: boolean) => {
    const f = feedback();
    if (!f?.id || !f.conflict_with) return;
    if (keepNew) await ipc.rulesSetPriority(f.id, f.conflict_with);
    else await ipc.rulesSetPriority(f.conflict_with, f.id);
    setFeedback(null);
    setDraft("");
    setEditing(null);
    refetch();
    refreshSettings();
  };

  return (
    <div>
      <div class="settings-h1">{label("rules.prompt", settings()?.voice)}</div>

      <div class="rule-editor">
        <textarea
          placeholder={label("rules.placeholder", settings()?.voice)}
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
        />
        <div class="foot">
          <button class="btn" onClick={submit} disabled={busy() || !draft().trim()}>
            {editing() ? "Modifier" : "Ajouter"}
          </button>
        </div>
      </div>

      <Show when={feedback()} keyed>
        {(f) => (
          <div class="rule-feedback fade-in" classList={{ refused: f.status === "refused" }}>
            <Show when={f.status === "refused"}>
              <b>Règle refusée.</b> {f.reason}
            </Show>
            <Show when={f.status === "conflict"}>
              <b>Conflit détecté.</b> {f.reason}
              <div style={{ display: "flex", gap: "8px", "margin-top": "8px" }}>
                <button class="btn primary" onClick={() => resolveConflict(true)}>
                  Privilégier la nouvelle
                </button>
                <button class="btn" onClick={() => resolveConflict(false)}>
                  Garder l'ancienne
                </button>
              </div>
            </Show>
            <Show when={f.status === "active"}>
              Règle enregistrée
              {f.kind === "style" && " — appliquée au comportement de Syn."}
              {f.kind === "standing" && " — programmée en tâche de fond (voir Mes programmations)."}
              {f.kind === "action_modifier" && " — appliquée quand l'action concernée sera composée."}
            </Show>
          </div>
        )}
      </Show>

      <div class="rules-title">{label("rules.title", settings()?.voice)}</div>
      <Show when={(rules() ?? []).length === 0}>
        <div class="muted">Aucune règle pour l'instant.</div>
      </Show>
      <For each={rules() ?? []}>
        {(r) => (
          <div class="rule-line">
            <span class="grow" title={r.text}>
              {r.text}
              <Show when={r.status === "conflict"}>
                <span style={{ color: "var(--warn)" }}> · en conflit</span>
              </Show>
            </span>
            <button
              title="Modifier"
              onClick={() => {
                setEditing(r);
                setDraft(r.text);
                setFeedback(null);
              }}
            >
              <Icon name="square-pen" size={15} />
            </button>
            <button
              title="Supprimer"
              onClick={async () => {
                await ipc.rulesDelete(r.id);
                refetch();
                refreshSettings();
              }}
            >
              <Icon name="circle-x" size={15} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
