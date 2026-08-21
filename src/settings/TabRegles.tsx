// Onglet Règles (maquette Réglages) : éditeur + « Vos règles » (tu/vous templaté).
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc, type Rule, type RuleOutcome } from "../lib/ipc";
import { label } from "../lib/voice";
import { settings, refreshSettings } from "../lib/state";

interface MailRuleParams {
  action: "archive" | "trash" | "keep";
  provider?: "google" | "microsoft" | null;
  sender_terms: string[];
  topics: string[];
}

function mailRuleSummary(rule: Rule): string | null {
  if (rule.kind !== "mail_cleanup" || !rule.params || typeof rule.params !== "object") return null;
  const params = rule.params as Partial<MailRuleParams>;
  const action = { archive: "Archiver", trash: "Mettre à la corbeille", keep: "Conserver" }[params.action ?? ""];
  if (!action) return null;
  const topics: Record<string, string> = {
    invoice: "factures et reçus",
    booking: "réservations et billets",
    marketing: "communications marketing",
    notification: "notifications",
  };
  const criteria = [
    ...(params.topics ?? []).map((topic) => topics[topic] ?? topic),
    (params.sender_terms ?? []).length ? `expéditeur : ${(params.sender_terms ?? []).join(" ")}` : "",
    params.provider === "google" ? "Gmail uniquement" : params.provider === "microsoft" ? "Outlook uniquement" : "",
  ].filter(Boolean);
  return `${action} · ${criteria.join(" · ")}`;
}

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
              {f.kind === "style" && " et appliquée au comportement de Syn."}
              {f.kind === "standing" && " et ajoutée à Mes programmations."}
              {f.kind === "action_modifier" && " et appliquée aux actions concernées."}
              {f.kind === "mail_cleanup" && " et prioritaire lors du rangement de tes mails."}
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
              <Show when={mailRuleSummary(r)} keyed>
                {(summary) => <small class="muted" style={{ display: "block" }}>{summary}</small>}
              </Show>
              <Show when={r.status === "conflict"}>
                <span style={{ color: "var(--warn)" }}> (en conflit)</span>
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
              <Icon name="x" size={15} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
