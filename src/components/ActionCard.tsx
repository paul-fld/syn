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

interface PlanMove {
  from: string;
  to: string;
  reason?: string;
}

interface ReorganizePlan {
  target_dir: string;
  moves: PlanMove[];
  quarantine: PlanMove[];
  ambiguous: Array<{ path: string; question?: string }>;
  untouched: Array<{ path: string; reason?: string }>;
  summary: string;
}

function planFromInput(input: unknown): ReorganizePlan | null {
  const plan = (input as { plan?: ReorganizePlan } | null)?.plan;
  return plan && Array.isArray(plan.moves) && typeof plan.target_dir === "string" ? plan : null;
}

function fileName(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function relativePath(path: string, root: string): string {
  const prefix = root.endsWith("/") ? root : `${root}/`;
  return path.startsWith(prefix) ? path.slice(prefix.length) : path;
}

export function ReorganizePlanView(props: { input: unknown; compact?: boolean }): JSX.Element {
  const plan = () => planFromInput(props.input);
  const allMoves = () => [...(plan()?.moves ?? []), ...(plan()?.quarantine ?? [])];
  const folders = () => new Set(allMoves().map((move) => {
    const relative = relativePath(move.to, plan()?.target_dir ?? "");
    return relative.split("/").slice(0, -1).join("/") || "Racine";
  })).size;

  return (
    <Show when={plan()} keyed>
      {(current) => (
        <div class="reorganize-plan" classList={{ compact: !!props.compact }}>
          <div class="reorganize-plan-stats">
            <span><strong>{allMoves().length}</strong> déplacements</span>
            <span><strong>{folders()}</strong> dossiers de classement</span>
            <Show when={current.ambiguous.length > 0}>
              <span><strong>{current.ambiguous.length}</strong> à décider</span>
            </Show>
            <Show when={current.untouched.length > 0}>
              <span><strong>{current.untouched.length}</strong> laissés en place</span>
            </Show>
          </div>
          <details class="reorganize-plan-details" open={!props.compact}>
            <summary>Voir le plan détaillé</summary>
            <div class="reorganize-moves">
              <For each={current.moves}>
                {(move) => (
                  <div class="reorganize-move">
                    <span class="reorganize-file" title={move.from}>{fileName(move.from)}</span>
                    <Icon name="chevron-right" size={12} />
                    <span class="reorganize-destination" title={move.to}>{relativePath(move.to, current.target_dir)}</span>
                    <Show when={move.reason}><span class="reorganize-reason">{move.reason}</span></Show>
                  </div>
                )}
              </For>
              <For each={current.quarantine}>
                {(move) => (
                  <div class="reorganize-move quarantine">
                    <span class="reorganize-file" title={move.from}>{fileName(move.from)}</span>
                    <Icon name="chevron-right" size={12} />
                    <span class="reorganize-destination">À vérifier avant suppression</span>
                  </div>
                )}
              </For>
              <For each={current.ambiguous}>
                {(item) => (
                  <div class="reorganize-move undecided">
                    <span class="reorganize-file" title={item.path}>{fileName(item.path)}</span>
                    <span class="reorganize-destination">Non déplacé · décision nécessaire</span>
                  </div>
                )}
              </For>
              <For each={current.untouched}>
                {(item) => (
                  <div class="reorganize-move untouched">
                    <span class="reorganize-file" title={item.path}>{fileName(item.path)}</span>
                    <span class="reorganize-destination">Laissé en place</span>
                    <Show when={item.reason}><span class="reorganize-reason">{item.reason}</span></Show>
                  </div>
                )}
              </For>
            </div>
          </details>
        </div>
      )}
    </Show>
  );
}

export function ActionCard(props: { action: PendingAction }): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const [result, setResult] = createSignal<unknown>(null);
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
      setResult(r);
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
      <Show when={props.action.tool === "files.apply_reorganize_plan"}>
        <ReorganizePlanView input={props.action.input} />
      </Show>
      <Show when={busy()}>
        <div class="action-working-shimmer" role="status">
          <span>Syn applique et vérifie chaque déplacement…</span>
        </div>
      </Show>
      <Show when={busy() && progress().length > 0}>
        <div class="agent-progress-list">
          <For each={progress()}>
            {(step) => <div class={`agent-progress-step ${step.status}`}>{step.title}<Show when={step.detail}><span class="sub"> : {step.detail}</span></Show></div>}
          </For>
        </div>
      </Show>
      <Show
        when={result() == null && !error()}
        fallback={
          <div class="sub" style={{ color: error() ? "var(--danger)" : "var(--ok)" }}>
            {error() ?? "Action exécutée et vérifiée."}
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
