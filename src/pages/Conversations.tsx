// Conversations : historique + fil sourcé + confirmations d'actions dans le fil.
import { createEffect, createResource, createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { AskBar } from "../components/AskBar";
import { ActionCard } from "../components/ActionCard";
import { Icon } from "../components/Icon";
import { ipc, on, type AgentProgress, type Answer, type Retrieved, type PendingAction } from "../lib/ipc";
import { barQuery, setBarQuery, refreshPending, pendingActions, sessionsVersion } from "../lib/state";

interface Msg {
  role: "user" | "assistant";
  content: string;
  sources?: Retrieved[];
  degraded?: boolean;
}

export function Conversations(): JSX.Element {
  const [sessions, { refetch: refetchSessions }] = createResource(sessionsVersion, () => ipc.listSessions());
  const [sessionId, setSessionId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Msg[]>([]);
  const [thinking, setThinking] = createSignal(false);
  const [progress, setProgress] = createSignal<AgentProgress[]>([]);
  let threadEl: HTMLDivElement | undefined;

  const scrollDown = () => setTimeout(() => threadEl?.scrollTo({ top: threadEl.scrollHeight }), 30);

  const openSession = async (id: string) => {
    setSessionId(id);
    const turns = await ipc.getConversation(id);
    setMessages(turns.map((t: any) => ({ role: t.role, content: t.content })));
    scrollDown();
  };

  const send = async (text: string) => {
    const sid = sessionId() ?? crypto.randomUUID();
    setSessionId(sid);
    setProgress([]);
    setMessages((m) => [...m, { role: "user", content: text }]);
    setThinking(true);
    scrollDown();
    try {
      const answer: Answer = await ipc.query(sid, text);
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
      send(q);
    }
  });
  createEffect(() => {
    const q = barQuery();
    if (q) {
      setBarQuery(null);
      setSessionId(null);
      setMessages([]);
      send(q);
    }
  });

  const sessionPending = (): PendingAction[] =>
    pendingActions().filter((a) => !a.session_id || a.session_id === sessionId());

  return (
    <div class="convo-layout">
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
        <For each={sessions() ?? []}>
          {(s: any) => (
            <button
              class="convo-list-item"
              classList={{ active: sessionId() === s.id }}
              onClick={() => openSession(s.id)}
            >
              {s.title || "Sans titre"}
            </button>
          )}
        </For>
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
    </div>
  );
}
