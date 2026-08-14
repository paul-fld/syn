// Confirmation d'action (plancher humain) : claire, explicite, jamais pré-cochée.
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Icon } from "./Icon";
import { ipc, on, type AgentProgress, type PendingAction } from "../lib/ipc";
import { refreshPending } from "../lib/state";

const RISK_LABEL: Record<string, string> = {
  floor: "Confirmation obligatoire",
  reversible_external: "Action externe annulable",
  reversible_local: "Action locale annulable",
  read: "Lecture",
};

export function ActionCard(props: { action: PendingAction }): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const [result, setResult] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [progress, setProgress] = createSignal<AgentProgress[]>([]);

  onMount(() => {
    let unlisten: (() => void) | undefined;
    void on("agent_progress", (raw) => {
      const event = (raw?.payload ?? raw) as AgentProgress;
      if (props.action.session_id && event.session_id === props.action.session_id) {
        setProgress((steps) => [...steps, event].slice(-8));
      }
    }).then((fn) => (unlisten = fn));
    onCleanup(() => unlisten?.());
  });

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await ipc.confirmAction(props.action.id);
      setResult(typeof r === "string" ? r : JSON.stringify(r));
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setBusy(false);
      refreshPending();
    }
  };
  const reject = async () => {
    setBusy(true);
    try {
      await ipc.rejectAction(props.action.id);
    } finally {
      setBusy(false);
      refreshPending();
    }
  };

  return (
    <div class="action-card fade-in">
      <div class="risk" classList={{ untrusted: props.action.derived_from_untrusted }}>
        <Icon name="shield" size={12} />
        {RISK_LABEL[props.action.risk_class] ?? props.action.risk_class}
        <Show when={props.action.derived_from_untrusted}>
          <span>· dérivé de contenu non fiable</span>
        </Show>
      </div>
      <div class="preview">{props.action.preview}</div>
      <Show when={busy() && progress().length > 0}>
        <div class="agent-progress-list">
          <For each={progress()}>
            {(step) => <div class={`agent-progress-step ${step.status}`}>{step.title}<Show when={step.detail}><span class="sub"> : {step.detail}</span></Show></div>}
          </For>
        </div>
      </Show>
      <Show
        when={!result() && !error()}
        fallback={
          <div class="sub" style={{ color: error() ? "var(--danger)" : "var(--ok)" }}>
            {error() ?? "Exécuté. " + (result() ?? "")}
          </div>
        }
      >
        <div class="buttons">
          <button class="btn primary" disabled={busy()} onClick={confirm}>
            {busy() ? "Exécution…" : "Confirmer"}
          </button>
          <button class="btn" disabled={busy()} onClick={reject}>
            Refuser
          </button>
        </div>
      </Show>
    </div>
  );
}
