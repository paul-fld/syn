// Conversations : historique + fil sourcé + confirmations d'actions dans le fil.
import { createEffect, createResource, createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { AskBar } from "../components/AskBar";
import { ActionCard } from "../components/ActionCard";
import { Icon } from "../components/Icon";
import { ipc, on, type AgentProgress, type Answer, type Retrieved, type PendingAction, type ScreenContext, type ConversationSession } from "../lib/ipc";
import { barQuery, setBarQuery, refreshPending, pendingActions, sessionsVersion, settings } from "../lib/state";

interface Msg {
  role: "user" | "assistant";
  content: string;
  sources?: Retrieved[];
  degraded?: boolean;
}

type ConversationDialog = {
  kind: "rename" | "new-project" | "create-project" | "delete";
  session?: ConversationSession;
};

type LinkableSource = Pick<Retrieved, "title" | "source_ref" | "path">;

function sourceTarget(source: LinkableSource): string {
  return source.path ?? source.source_ref;
}

function InlineMessage(props: { text: string; sources?: LinkableSource[] }): JSX.Element {
  const parts: JSX.Element[] = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`)/g;
  let cursor = 0;
  for (const match of props.text.matchAll(pattern)) {
    const start = match.index ?? 0;
    if (start > cursor) parts.push(props.text.slice(cursor, start));
    const token = match[0];
    const label = token.startsWith("**") ? token.slice(2, -2) : "";
    const source = label
      ? props.sources?.find((candidate) => candidate.title === label)
      : undefined;
    parts.push(
      source ? (
        <button
          type="button"
          class="inline-source-link"
          title={sourceTarget(source)}
          aria-label={`Ouvrir ${label}`}
          onClick={() => ipc.openSource(sourceTarget(source)).catch(() => {})}
        >
          {label}
        </button>
      ) : token.startsWith("**") ? (
        <strong>{label}</strong>
      ) : (
        <code>{token.slice(1, -1)}</code>
      ),
    );
    cursor = start + token.length;
  }
  if (cursor < props.text.length) parts.push(props.text.slice(cursor));
  return <>{parts}</>;
}

function embeddedSource(raw: string, sources?: Retrieved[]): LinkableSource | undefined {
  const numbered = raw.match(/^\s*(\d+)[.)]\s+/);
  const structured = raw
    .replace(/^\s*(?:[*-]|\d+[.)])\s+/, "")
    .match(/^\*\*(.+?)\*\*\s+—\s+(.+)$/);
  if (!structured) return undefined;
  const fromAnswer = numbered ? sources?.[Number(numbered[1]) - 1] : undefined;
  return fromAnswer ?? { title: structured[1], source_ref: structured[2], path: structured[2] };
}

function sourcesAreLinkedInline(content: string, sources?: Retrieved[]): boolean {
  return !!sources?.length && sources.every((source) =>
    !!source.title && content.includes(`**${source.title}**`),
  );
}

function MessageContent(props: { content: string; sources?: Retrieved[] }): JSX.Element {
  return (
    <div class="msg-content">
      <For each={props.content.split("\n")}>
        {(raw) => {
          const bullet = /^\s*[*-]\s+/.test(raw);
          const numbered = /^\s*\d+[.)]\s+/.test(raw);
          const text = raw.replace(/^\s*(?:[*-]|\d+[.)])\s+/, "");
          const lineSource = embeddedSource(raw, props.sources);
          return (
            <div class="msg-line" classList={{ bullet, numbered, empty: !raw }}>
              <Show when={raw} fallback={<br />}>
                <InlineMessage
                  text={text}
                  sources={lineSource ? [lineSource] : props.sources}
                />
              </Show>
            </div>
          );
        }}
      </For>
    </div>
  );
}

export function Conversations(): JSX.Element {
  const [sessions, { refetch: refetchSessions }] = createResource(sessionsVersion, () => ipc.listSessions());
  const [projects, { refetch: refetchProjects }] = createResource(() => ipc.listProjects());
  const [sessionId, setSessionId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Msg[]>([]);
  const [thinking, setThinking] = createSignal(false);
  /// Réponse en cours d'écriture, affichée avant que le tour soit terminé.
  const [streaming, setStreaming] = createSignal("");
  const [progress, setProgress] = createSignal<AgentProgress[]>([]);
  const [openMenu, setOpenMenu] = createSignal<string | null>(null);
  const [moveMenu, setMoveMenu] = createSignal<string | null>(null);
  const [dialog, setDialog] = createSignal<ConversationDialog | null>(null);
  const [dialogValue, setDialogValue] = createSignal("");
  const [dialogError, setDialogError] = createSignal("");
  let threadEl: HTMLDivElement | undefined;

  const scrollDown = () => setTimeout(() => threadEl?.scrollTo({ top: threadEl.scrollHeight }), 30);

  const openSession = async (id: string) => {
    setSessionId(id);
    const turns = await ipc.getConversation(id);
    setMessages(turns.map((t: any) => ({ role: t.role, content: t.content })));
    scrollDown();
  };

  const send = async (text: string, screenContext?: ScreenContext | null) => {
    const sid = sessionId() ?? crypto.randomUUID();
    setSessionId(sid);
    setProgress([]);
    setStreaming("");
    setMessages((m) => [...m, { role: "user", content: text }]);
    setThinking(true);
    scrollDown();
    try {
      const answer: Answer = await ipc.query(sid, text, screenContext);
      setSessionId(answer.session_id);
      setMessages((m) => [
        ...m,
        { role: "assistant", content: answer.text, sources: answer.sources, degraded: answer.degraded },
      ]);
      refetchSessions();
      refreshPending();
    } catch (e: any) {
      setMessages((m) => [...m, { role: "assistant", content: `⚠ ${e?.message ?? e}`, degraded: true }]);
    } finally {
      setThinking(false);
      setStreaming("");
      scrollDown();
    }
  };

  // Requête venue de la barre d'interaction ou de l'Accueil.
  onMount(() => {
    let unlistenProgress: (() => void) | undefined;
    let unlistenResolved: (() => void) | undefined;
    let unlistenSemantic: (() => void) | undefined;
    let unlistenDelta: (() => void) | undefined;
    void on("agent_progress", (raw) => {
      const event = (raw?.payload ?? raw) as AgentProgress;
      if (event.session_id !== sessionId()) return;
      setProgress((steps) => [...steps, event].slice(-20));
      scrollDown();
    }).then((fn) => (unlistenProgress = fn));
    // Réponse en cours d'écriture : on affiche les fragments dès qu'ils
    // arrivent au lieu d'attendre le bloc final. Le message est remplacé par la
    // version définitive quand `ipc.query` rend la main — les sources et le
    // texte canonique viennent de là.
    void on("answer_delta", (raw) => {
      const event = (raw?.payload ?? raw) as { session_id: string; delta: string };
      if (event.session_id !== sessionId() || !event.delta) return;
      setStreaming((current) => current + event.delta);
      scrollDown();
    }).then((fn) => (unlistenDelta = fn));
    void on("semantic_results", (raw) => {
      const event = (raw?.payload ?? raw) as { session_id: string; results: Retrieved[] };
      if (event.session_id !== sessionId() || !event.results?.length) return;
      setMessages((current) => {
        const index = current.findLastIndex((message) => message.role === "assistant");
        if (index < 0) return current;
        const next = [...current];
        const existing = next[index].sources ?? [];
        const seen = new Set(existing.map((source) => source.item_id));
        next[index] = {
          ...next[index],
          sources: [...existing, ...event.results.filter((source) => !seen.has(source.item_id))],
        };
        return next;
      });
      scrollDown();
    }).then((fn) => (unlistenSemantic = fn));
    void on("action_resolved", async () => {
      const active = sessionId();
      if (!active) return;
      const turns = await ipc.getConversation(active).catch(() => null);
      if (turns) {
        setMessages(turns.map((turn: any) => ({ role: turn.role, content: turn.content })));
        scrollDown();
      }
    }).then((fn) => (unlistenResolved = fn));
    onCleanup(() => {
      unlistenProgress?.();
      unlistenResolved?.();
      unlistenSemantic?.();
      unlistenDelta?.();
    });
  });
  // Requête venue de la barre ou de l'Accueil (l'effet couvre aussi le montage).
  createEffect(() => {
    const q = barQuery();
    if (q) {
      setBarQuery(null);
      setSessionId(null);
      setMessages([]);
      send(q.text, q.screenContext);
    }
  });

  const sessionPending = (): PendingAction[] =>
    pendingActions().filter((a) => !a.session_id || a.session_id === sessionId());

  const refreshLists = async () => {
    await Promise.all([refetchSessions(), refetchProjects()]);
  };

  const moveToProject = async (session: ConversationSession, projectId: string | null) => {
    await ipc.moveSessionToProject(session.id, projectId);
    setOpenMenu(null);
    setMoveMenu(null);
    await refreshLists();
  };

  const submitDialog = async () => {
    const current = dialog();
    if (!current) return;
    setDialogError("");
    try {
      if (current.kind === "rename") {
        await ipc.renameSession(current.session!.id, dialogValue());
      } else if (current.kind === "new-project") {
        const project = await ipc.createProject(dialogValue());
        await ipc.moveSessionToProject(current.session!.id, project.id);
      } else if (current.kind === "create-project") {
        await ipc.createProject(dialogValue());
      } else {
        await ipc.deleteSession(current.session!.id);
        await refreshPending();
        if (sessionId() === current.session!.id) {
          setSessionId(null);
          setMessages([]);
        }
      }
      setDialog(null);
      setOpenMenu(null);
      setMoveMenu(null);
      await refreshLists();
    } catch (e: any) {
      setDialogError(e?.message ?? String(e));
    }
  };

  const sessionRow = (s: ConversationSession): JSX.Element => (
    <div class="convo-list-row" classList={{ active: sessionId() === s.id }}>
      <button class="convo-list-item" onClick={() => openSession(s.id)} title={s.title || "Sans titre"}>
        {s.title || "Sans titre"}
      </button>
      <button
        class="convo-more"
        title="Actions sur la conversation"
        aria-label={`Actions pour ${s.title || "cette conversation"}`}
        aria-expanded={openMenu() === s.id}
        onClick={(event) => {
          event.stopPropagation();
          setOpenMenu(openMenu() === s.id ? null : s.id);
          setMoveMenu(null);
        }}
      >
        <Icon name="ellipsis" size={15} />
      </button>
      <Show when={openMenu() === s.id}>
        <div class="conversation-menu" onClick={(event) => event.stopPropagation()}>
          <button onClick={() => { setDialogValue(s.title || ""); setDialog({ kind: "rename", session: s }); }}>
            <Icon name="square-pen" size={14} /> Renommer
          </button>
          <button onClick={() => setMoveMenu(moveMenu() === s.id ? null : s.id)}>
            <Icon name="folder-input" size={14} /> Déplacer dans un projet
            <Icon name="chevron-right" size={12} />
          </button>
          <Show when={moveMenu() === s.id}>
            <div class="conversation-project-choices">
              <button classList={{ selected: !s.project_id }} onClick={() => moveToProject(s, null)}>
                Sans projet
              </button>
              <For each={projects() ?? []}>
                {(project) => (
                  <button classList={{ selected: s.project_id === project.id }} onClick={() => moveToProject(s, project.id)}>
                    <Icon name="folder" size={12} /> {project.name}
                  </button>
                )}
              </For>
              <button onClick={() => { setDialogValue(""); setDialog({ kind: "new-project", session: s }); }}>
                <Icon name="plus" size={12} /> Nouveau projet…
              </button>
            </div>
          </Show>
          <button
            class="danger"
            disabled={thinking() && sessionId() === s.id}
            title={thinking() && sessionId() === s.id ? "Attends la fin de la réponse avant de supprimer ce fil" : undefined}
            onClick={() => { setDialogValue(""); setDialog({ kind: "delete", session: s }); }}
          >
            <Icon name="x" size={14} /> Supprimer
          </button>
        </div>
      </Show>
    </div>
  );

  return (
    <div class="convo-layout" onClick={() => { setOpenMenu(null); setMoveMenu(null); }}>
      <div class="convo-list">
        <button
          class="convo-list-item"
          onClick={() => {
            setSessionId(null);
            setMessages([]);
          }}
        >
          ＋ Nouvelle conversation
        </button>
        <button
          class="convo-new-project"
          onClick={() => {
            setDialogValue("");
            setDialogError("");
            setDialog({ kind: "create-project" });
          }}
        >
          <Icon name="folder" size={13} />
          Nouveau projet
          <Icon name="plus" size={13} />
        </button>
        <For each={projects() ?? []}>
          {(project) => (
            <>
              <div class="convo-project-label"><Icon name="folder" size={12} /> {project.name}</div>
              <For each={(sessions() ?? []).filter((s) => s.project_id === project.id)}>{sessionRow}</For>
            </>
          )}
        </For>
        <Show when={(projects()?.length ?? 0) > 0 && (sessions() ?? []).some((s) => !s.project_id)}>
          <div class="convo-project-label muted">Conversations</div>
        </Show>
        <For each={(sessions() ?? []).filter((s) => !s.project_id)}>{sessionRow}</For>
      </div>

      <div class="convo-main">
        <div class="convo-thread" ref={threadEl} aria-live="polite" aria-busy={thinking()}>
          <Show when={messages().length === 0 && !thinking()}>
            <div class="empty-note" style={{ "margin-top": "80px" }}>
              Pose une question sur tes documents, tes mails, ton agenda ou ta machine.
            </div>
          </Show>
          <For each={messages()}>
            {(m) => (
              <div class="msg fade-in" classList={{ user: m.role === "user", assistant: m.role === "assistant" }}>
                <MessageContent content={m.content} sources={m.sources} />
                <Show when={m.role === "assistant" && settings()?.voice_output_enabled && m.content}>
                  <button
                    class="source-pill"
                    title="Lire à voix haute"
                    aria-label="Lire à voix haute"
                    style={{ "margin-left": "8px" }}
                    onClick={() => ipc.speakText(m.content).catch(() => {})}
                  >
                    <Icon name="audio-lines" size={12} />
                  </button>
                </Show>
                <Show when={m.sources && m.sources.length > 0 && !sourcesAreLinkedInline(m.content, m.sources)}>
                  <div class="sources">
                    <For each={m.sources}>
                      {(s, i) => (
                        <button
                          class="source-pill"
                          title={s.source_ref}
                          disabled={!s.path && !s.source_ref}
                          onClick={() => ipc.openSource(s.path ?? s.source_ref).catch(() => {})}
                        >
                          [{i() + 1}] {s.title || s.source_ref}
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            )}
          </For>
          <Show when={sessionPending().length > 0}>
            <For each={sessionPending()}>{(a) => <ActionCard action={a} />}</For>
          </Show>
          <Show when={streaming()}>
            <div class="msg assistant is-streaming">
              <InlineMessage text={streaming()} />
            </div>
          </Show>
          <Show when={thinking()}>
            <details class="agent-progress is-working" open>
              <summary>
                <span class="dot" />
                <span class="agent-progress-title">
                  {progress()[progress().length - 1]?.title ?? "Syn analyse la demande…"}
                </span>
              </summary>
              <div class="agent-progress-list">
                <For each={progress()}>
                  {(step) => (
                    <div class={`agent-progress-step ${step.status}`}>
                      <Icon name={step.status === "done" ? "check" : step.status === "error" ? "x" : "corner-down-right"} size={12} />
                      <span>
                        {step.title}
                        <Show when={step.detail}><span class="sub"> : {step.detail}</span></Show>
                      </span>
                    </div>
                  )}
                </For>
              </div>
            </details>
          </Show>
        </div>
        <div class="convo-input-zone">
          <AskBar onSubmit={send} disabled={thinking()} autofocus />
        </div>
      </div>
      <Show when={dialog()}>
        <div class="conversation-dialog-backdrop" onClick={() => setDialog(null)}>
          <div class="conversation-dialog" role="dialog" aria-modal="true" aria-labelledby="conversation-dialog-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="conversation-dialog-title">
              {dialog()!.kind === "rename"
                ? "Renommer la conversation"
                : dialog()!.kind === "delete"
                  ? "Supprimer la conversation ?"
                  : "Créer un projet"}
            </h3>
            <Show
              when={dialog()!.kind !== "delete"}
              fallback={<p>« {dialog()!.session?.title || "Sans titre"} » sera supprimée définitivement. Le journal d’activité restera conservé.</p>}
            >
              <input
                class="text-input"
                aria-label={dialog()!.kind === "rename" ? "Titre de la conversation" : "Nom du projet"}
                value={dialogValue()}
                placeholder={dialog()!.kind === "rename" ? "Titre de la conversation" : "Nom du projet"}
                autofocus
                onInput={(event) => setDialogValue(event.currentTarget.value)}
                onKeyDown={(event) => event.key === "Enter" && submitDialog()}
              />
            </Show>
            <Show when={dialogError()}><div class="conversation-dialog-error">{dialogError()}</div></Show>
            <div class="conversation-dialog-actions">
              <button class="btn" onClick={() => setDialog(null)}>Annuler</button>
              <button class="btn primary" classList={{ danger: dialog()!.kind === "delete" }} onClick={submitDialog}>
                {dialog()!.kind === "rename"
                  ? "Renommer"
                  : dialog()!.kind === "new-project"
                    ? "Créer et déplacer"
                    : dialog()!.kind === "create-project"
                      ? "Créer"
                      : "Supprimer"}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
