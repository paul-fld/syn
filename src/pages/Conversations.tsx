// Conversations : historique + fil sourcé + confirmations d'actions dans le fil.
import { createEffect, createResource, createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { AskBar } from "../components/AskBar";
import { ActionCard } from "../components/ActionCard";
import { Icon } from "../components/Icon";
import { ipc, on, type AgentProgress, type Answer, type Retrieved, type PendingAction, type ScreenContext, type ConversationSession } from "../lib/ipc";
import { barQuery, setBarQuery, refreshPending, pendingActions, sessionsVersion } from "../lib/state";

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

export function Conversations(): JSX.Element {
  const [sessions, { refetch: refetchSessions }] = createResource(sessionsVersion, () => ipc.listSessions());
  const [projects, { refetch: refetchProjects }] = createResource(() => ipc.listProjects());
  const [sessionId, setSessionId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Msg[]>([]);
  const [thinking, setThinking] = createSignal(false);
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
      scrollDown();
    }
  };

  // Requête venue de la barre d'interaction ou de l'Accueil.
  onMount(() => {
    let unlisten: (() => void) | undefined;
    void on("agent_progress", (raw) => {
      const event = (raw?.payload ?? raw) as AgentProgress;
      if (event.session_id !== sessionId()) return;
      setProgress((steps) => [...steps, event].slice(-20));
      scrollDown();
    }).then((fn) => (unlisten = fn));
    onCleanup(() => unlisten?.());
    const q = barQuery();
    if (q) {
      setBarQuery(null);
      setSessionId(null);
      setMessages([]);
      send(q.text, q.screenContext);
    }
  });
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
            <Icon name="circle-x" size={14} /> Supprimer
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
        <div class="convo-thread" ref={threadEl}>
          <Show when={messages().length === 0 && !thinking()}>
            <div class="empty-note" style={{ "margin-top": "80px" }}>
              Pose une question sur tes documents, tes mails, ton agenda ou ta machine.
              <br />
              Chaque réponse cite ses sources — tout reste sur cette machine.
            </div>
          </Show>
          <For each={messages()}>
            {(m) => (
              <div class="msg fade-in" classList={{ user: m.role === "user", assistant: m.role === "assistant" }}>
                {m.content}
                <Show when={m.sources && m.sources.length > 0}>
                  <div class="sources">
                    <For each={m.sources}>
                      {(s, i) => (
                        <button
                          class="source-pill"
                          title={s.source_ref}
                          disabled={!s.path}
                          onClick={() => s.path && ipc.openSource(s.path).catch(() => {})}
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
          <Show when={thinking()}>
            <details class="agent-progress" open>
              <summary>
                <span class="dot" />
                {progress()[progress().length - 1]?.title ?? "Démarrage du traitement local…"}
              </summary>
              <div class="agent-progress-list">
                <For each={progress()}>
                  {(step) => (
                    <div class={`agent-progress-step ${step.status}`}>
                      <Icon name={step.status === "done" ? "check" : step.status === "error" ? "circle-x" : "corner-down-right"} size={12} />
                      <span>
                        {step.title}
                        <Show when={step.detail}><span class="sub"> — {step.detail}</span></Show>
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
          <div class="conversation-dialog" onClick={(event) => event.stopPropagation()}>
            <h3>
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
