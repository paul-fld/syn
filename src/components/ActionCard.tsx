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

interface MailSend {
  to: string;
  subject: string;
  body: string;
  via: string;
}

function mailFromInput(input: unknown): MailSend | null {
  const mail = input as Partial<MailSend> | null;
  if (!mail || typeof mail.to !== "string" || typeof mail.body !== "string") return null;
  return { to: mail.to, subject: mail.subject ?? "", body: mail.body, via: mail.via ?? "apple" };
}

const ACCOUNT: Record<string, { label: string; icon: string }> = {
  google: { label: "Gmail", icon: "gmail" },
  microsoft: { label: "Outlook", icon: "outlook" },
  apple: { label: "Apple Mail", icon: "apple-mail" },
};

/// Le mail tel qu'il partira : destinataire, compte, objet, et le texte entier.
///
/// L'aperçu générique écrasait le message sur une ligne et le coupait à 500
/// caractères — on confirmait un envoi sans pouvoir le relire. Ici le corps
/// garde ses paragraphes.
function MailSendView(props: { input: unknown }): JSX.Element {
  const mail = () => mailFromInput(props.input);
  const account = () => ACCOUNT[mail()?.via ?? "apple"] ?? ACCOUNT.apple;
  return (
    <Show when={mail()} keyed>
      {(current) => (
        <div class="mail-preview">
          <div class="mail-preview-head">
            <span class="mail-preview-account">
              <Icon name={account().icon} size={15} />
              {account().label}
            </span>
            <span class="mail-preview-to">à {current.to}</span>
          </div>
          <Show when={current.subject}>
            <div class="mail-preview-subject">Objet : {current.subject}</div>
          </Show>
          <div class="mail-preview-body">
            <For each={current.body.split("\n")}>
              {(line) => <div classList={{ empty: !line.trim() }}>{line || " "}</div>}
            </For>
          </div>
        </div>
      )}
    </Show>
  );
}

interface MailCleanupPreview {
  provider: "google" | "microsoft";
  scanned: number;
  indexed: number;
  conversation_count?: number | null;
  unread_count?: number | null;
  archive_count: number;
  trash_count: number;
  unsubscribe_count: number;
  kept_count: number;
  review_count: number;
  untouched_count: number;
  rule_applied_count: number;
  deferred_count: number;
  archive_examples: Array<{ title: string; sender: string; reason: string }>;
  trash_examples: Array<{ title: string; sender: string; reason: string }>;
  unsubscribe_examples: Array<{ sender: string; message_count: number }>;
  top_bulk_senders: Array<[string, number]>;
  action_groups: Array<{ sender: string; action: "archive" | "trash"; reason: string; count: number }>;
}

function cleanupFromInput(input: unknown): MailCleanupPreview | null {
  const plan = (input as { plan?: MailCleanupPreview } | null)?.plan;
  return plan && (plan.provider === "google" || plan.provider === "microsoft") ? plan : null;
}

function MailCleanupPlanView(props: { input: unknown }): JSX.Element {
  const plan = () => cleanupFromInput(props.input);
  return (
    <Show when={plan()} keyed>
      {(current) => (
        <div class="reorganize-plan mail-cleanup-plan">
          <div class="reorganize-plan-stats">
            <span><strong>{current.scanned}</strong> messages recensés</span>
            <Show when={current.conversation_count != null && current.conversation_count !== current.scanned}>
              <span><strong>{current.conversation_count}</strong> conversations</span>
            </Show>
            <span><strong>{current.indexed}</strong> candidats inspectés</span>
            <span><strong>{current.archive_count}</strong> à archiver</span>
            <span><strong>{current.trash_count}</strong> à la corbeille</span>
            <span><strong>{current.unsubscribe_count}</strong> désabonnements</span>
            <span><strong>{current.kept_count}</strong> protégés après analyse</span>
            <span><strong>{current.review_count}</strong> cas ambigus</span>
            <span><strong>{current.untouched_count}</strong> laissés en place</span>
            <Show when={current.rule_applied_count > 0}>
              <span><strong>{current.rule_applied_count}</strong> classés par tes règles</span>
            </Show>
          </div>
          <details class="reorganize-plan-details">
            <summary>Voir des exemples et les principaux expéditeurs</summary>
            <div class="reorganize-moves">
              <For each={current.action_groups ?? []}>
                {(group) => (
                  <div class="reorganize-move">
                    <span class="reorganize-file" title={group.sender}>{group.sender}</span>
                    <span class="reorganize-destination">
                      {group.action === "trash" ? "Corbeille" : "Archiver"} · {group.count}
                    </span>
                    <span class="reorganize-reason">{group.reason}</span>
                  </div>
                )}
              </For>
              <For each={current.archive_examples}>
                {(mail) => (
                  <div class="reorganize-move">
                    <span class="reorganize-file" title={mail.title}>{mail.title}</span>
                    <span class="reorganize-destination">Archiver</span>
                    <span class="reorganize-reason">{mail.sender} · {mail.reason}</span>
                  </div>
                )}
              </For>
              <For each={current.trash_examples}>
                {(mail) => (
                  <div class="reorganize-move quarantine">
                    <span class="reorganize-file" title={mail.title}>{mail.title}</span>
                    <span class="reorganize-destination">Corbeille</span>
                    <span class="reorganize-reason">{mail.sender} · {mail.reason}</span>
                  </div>
                )}
              </For>
              <For each={current.unsubscribe_examples}>
                {(entry) => (
                  <div class="reorganize-move quarantine">
                    <span class="reorganize-file" title={entry.sender}>{entry.sender}</span>
                    <span class="reorganize-destination">Désabonnement définitif</span>
                    <span class="reorganize-reason">standard sécurisé « one click » · {entry.message_count} message(s)</span>
                  </div>
                )}
              </For>
            </div>
          </details>
          <Show when={current.unsubscribe_count > 0}>
            <div class="sub">Les déplacements de messages sont annulables. Un désabonnement confirmé ne peut pas être annulé automatiquement.</div>
          </Show>
          <Show when={current.deferred_count > 0}>
            <div class="sub">{current.deferred_count} message(s) seront traités lors d’un prochain passage afin de borner cette exécution.</div>
          </Show>
        </div>
      )}
    </Show>
  );
}

/// Ce que Syn est en train de faire, dit avec les mots de l'action en cours.
function workingLabel(tool: string): string {
  if (tool === "mail.cleanup.apply") return "Syn range et vérifie la boîte sélectionnée…";
  if (tool.startsWith("mail.")) return "Syn envoie le message…";
  if (tool.startsWith("document.")) return "Syn écrit le document…";
  if (tool.startsWith("calendar.")) return "Syn met l’agenda à jour…";
  if (tool.startsWith("tasks.")) return "Syn met tes tâches à jour…";
  if (tool.startsWith("people.")) return "Syn met à jour ce qu’il sait de tes contacts…";
  if (tool.startsWith("files.")) return "Syn applique et vérifie chaque déplacement…";
  return "Syn exécute l’action…";
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

  const isMail = () => props.action.tool === "mail.send";

  return (
    <div class="action-card fade-in" classList={{ mail: isMail() }}>
      {/* Un envoi de mail se relit ; il n'a pas besoin d'un bandeau qui
          rappelle sa classe de risque. L'avertissement de provenance, lui,
          reste affiché quoi qu'il arrive. */}
      <Show when={!isMail() || props.action.derived_from_untrusted}>
        <div class="risk" classList={{ untrusted: props.action.derived_from_untrusted }}>
          <Icon name="shield" size={12} />
          {RISK_LABEL[props.action.risk_class] ?? props.action.risk_class}
          <Show when={props.action.derived_from_untrusted}>
            <span>· dérivé de contenu non fiable</span>
          </Show>
        </div>
      </Show>
      <Show when={!isMail()}>
        <div class="preview">{props.action.preview}</div>
      </Show>
      <Show when={isMail()}>
        <MailSendView input={props.action.input} />
      </Show>
      <Show when={props.action.tool === "files.apply_reorganize_plan"}>
        <ReorganizePlanView input={props.action.input} />
      </Show>
      <Show when={props.action.tool === "mail.cleanup.apply"}>
        <MailCleanupPlanView input={props.action.input} />
      </Show>
      <Show when={busy()}>
        <div class="action-working-shimmer" role="status">
          {/* Le texte d'attente décrivait un rangement de fichiers, quelle que
              soit l'action : l'utilisateur voyait « Syn applique et vérifie
              chaque déplacement… » en envoyant un mail. */}
          <span>{workingLabel(props.action.tool)}</span>
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
