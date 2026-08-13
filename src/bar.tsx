/* Barre d'interaction autonome : converse et agit sans ouvrir la fenêtre principale. */
import { render } from "solid-js/web";
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { getCurrentWindow, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import { emitTo } from "@tauri-apps/api/event";
import "./styles/global.css";
import { Icon } from "./components/Icon";
import { SynGlyph } from "./components/Logo";
import { ipc, on, type AgentProgress, type PendingRef } from "./lib/ipc";

interface BarMessage {
  role: "user" | "assistant";
  content: string;
}

function Bar(): JSX.Element {
  const [text, setText] = createSignal("");
  const [messages, setMessages] = createSignal<BarMessage[]>([]);
  const [sessionId, setSessionId] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal<PendingRef[]>([]);
  const [thinking, setThinking] = createSignal(false);
  const [progress, setProgress] = createSignal<AgentProgress[]>([]);
  const [expanded, setExpanded] = createSignal(false);
  let inputEl: HTMLInputElement | undefined;

  const resize = async (open: boolean) => {
    const oldHeight = expanded() ? 390 : 64;
    setExpanded(open);
    const win = getCurrentWindow();
    const scale = await win.scaleFactor();
    const pos = await win.outerPosition();
    const newHeight = open ? 390 : 64;
    await win.setPosition(new LogicalPosition(pos.x / scale, pos.y / scale - (newHeight - oldHeight)));
    await win.setSize(new LogicalSize(560, newHeight));
  };

  onMount(() => {
    let unlistenProgress: (() => void) | undefined;
    let unlistenShown: (() => void) | undefined;
    void on("bar_shown", () => setTimeout(() => inputEl?.focus(), 60)).then((fn) => (unlistenShown = fn));
    void on("agent_progress", (raw) => {
      const event = (raw?.payload ?? raw) as AgentProgress;
      if (event.session_id === sessionId()) setProgress((steps) => [...steps, event].slice(-12));
    }).then((fn) => (unlistenProgress = fn));
    onCleanup(() => {
      unlistenProgress?.();
      unlistenShown?.();
    });
    setTimeout(() => inputEl?.focus(), 120);
  });

  const submit = async () => {
    const t = text().trim();
    if (!t || thinking()) return;
    const sid = sessionId() ?? crypto.randomUUID();
    setSessionId(sid);
    setProgress([]);
    setText("");
    setMessages((m) => [...m, { role: "user", content: t }]);
    setThinking(true);
    await resize(true);
    try {
      const answer = await ipc.query(sid, t);
      setSessionId(answer.session_id);
      setMessages((m) => [...m, { role: "assistant", content: answer.text }]);
      setPending(answer.pending_actions);
      await emitTo("main", "bar_conversation_updated", answer.session_id).catch(() => {});
    } catch (e: any) {
      setMessages((m) => [
        ...m,
        {
          role: "assistant",
          content: (e?.message ?? String(e)).includes("verrou")
            ? "Syn est verrouillé. Clique sur le logo pour ouvrir l’app et la déverrouiller."
            : `⚠ ${e?.message ?? e}`,
        },
      ]);
    } finally {
      setThinking(false);
      setTimeout(() => inputEl?.focus(), 30);
    }
  };

  const resolve = async (actionId: string, confirm: boolean) => {
    try {
      if (confirm) await ipc.confirmAction(actionId);
      else await ipc.rejectAction(actionId);
      setPending((xs) => xs.filter((x) => x.action_id !== actionId));
      setMessages((m) => [
        ...m,
        { role: "assistant", content: confirm ? "Action exécutée." : "Action annulée." },
      ]);
    } catch (e: any) {
      setMessages((m) => [...m, { role: "assistant", content: `⚠ ${e?.message ?? e}` }]);
    }
  };

  return (
    <div class="bar-shell" classList={{ expanded: expanded() }}>
      <Show when={expanded()}>
        <div class="bar-thread">
          <For each={messages()}>
            {(m) => <div class="bar-message" classList={{ user: m.role === "user" }}>{m.content}</div>}
          </For>
          <For each={pending()}>
            {(a) => (
              <div class="bar-action">
                <div>{a.preview}</div>
                <div class="bar-action-buttons">
                  <button class="btn primary" onClick={() => resolve(a.action_id, true)}>Confirmer</button>
                  <button class="btn" onClick={() => resolve(a.action_id, false)}>Refuser</button>
                </div>
              </div>
            )}
          </For>
          <Show when={thinking()}>
            <details class="agent-progress compact" open>
              <summary>{progress()[progress().length - 1]?.title ?? "Démarrage du traitement local…"}</summary>
              <div class="agent-progress-list">
                <For each={progress()}>{(step) => <div class={`agent-progress-step ${step.status}`}>{step.title}<Show when={step.detail}><span class="sub"> — {step.detail}</span></Show></div>}</For>
              </div>
            </details>
          </Show>
        </div>
      </Show>
      <div class="bar-pill">
        <button class="bar-logo" title="Ouvrir Syn" onClick={() => ipc.showMainWindow()}>
          <SynGlyph size={26} color="#c9c9cf" />
        </button>
        <input
          ref={inputEl}
          placeholder="Demander à Syn"
          value={text()}
          disabled={thinking()}
          onInput={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") ipc.hideBar();
          }}
        />
        <button title="Contexte d'écran" onClick={async () => {
          const ctx = await ipc.screenContext();
          if (ctx?.available) setText(`À propos de ${ctx.app}${ctx.window ? ` — ${ctx.window}` : ""} : `);
        }}>
          <Icon name="box-select" size={19} />
        </button>
        <button title="Réduire" onClick={() => resize(!expanded())}>
          <Icon name={expanded() ? "chevron-down" : "chevron-up"} size={19} />
        </button>
      </div>
    </div>
  );
}

render(() => <Bar />, document.getElementById("root")!);
