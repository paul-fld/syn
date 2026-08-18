/* Barre d'interaction autonome : converse et agit sans ouvrir la fenêtre principale. */
import { render } from "solid-js/web";
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { getCurrentWindow, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import { emitTo } from "@tauri-apps/api/event";
import "./styles/global.css";
import { Icon } from "./components/Icon";
import { SynGlyph } from "./components/Logo";
import { ipc, on, type AgentProgress, type PendingRef, type ScreenContext } from "./lib/ipc";
import { captureVisibleScreen } from "./lib/screenContext";

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
  const [screenContext, setScreenContext] = createSignal<ScreenContext | null>(null);
  const [capturing, setCapturing] = createSignal(false);
  const [captureError, setCaptureError] = createSignal("");
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
    const context = screenContext();
    setScreenContext(null);
    setMessages((m) => [...m, { role: "user", content: t }]);
    setThinking(true);
    await resize(true);
    try {
      const answer = await ipc.query(sid, t, context);
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
        <div class="bar-thread" aria-live="polite" aria-busy={thinking()}>
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
            <details class="agent-progress compact is-working" open>
              <summary>
                <span class="agent-progress-title">{progress()[progress().length - 1]?.title ?? "Syn analyse la demande…"}</span>
              </summary>
              <div class="agent-progress-list">
                <For each={progress()}>{(step) => <div class={`agent-progress-step ${step.status}`}>{step.title}<Show when={step.detail}><span class="sub"> : {step.detail}</span></Show></div>}</For>
              </div>
            </details>
          </Show>
        </div>
      </Show>
      <div class="bar-pill">
        <button
          class="bar-logo"
          title="Ouvrir Syn"
          aria-label="Ouvrir Syn"
          onClick={async () => {
            // Continuité barre → app : la saisie en cours suit l'utilisateur
            // dans la fenêtre principale (le canal bar_query était orphelin).
            const pending = text().trim();
            if (pending) await emitTo("main", "bar_query", pending).catch(() => {});
            await ipc.showMainWindow();
            if (pending) ipc.hideBar();
          }}
        >
          <SynGlyph size={26} color="#c9c9cf" />
        </button>
        <input
          aria-label="Demander à Syn"
          ref={inputEl}
          placeholder={captureError() ? "Capture impossible. Survole l’icône." : screenContext() ? "Contexte d’écran joint. Que veux-tu faire ?" : "Demander à Syn"}
          value={text()}
          disabled={thinking()}
          onInput={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") ipc.hideBar();
          }}
        />
        <button
          aria-label="Joindre le contexte visible à l’écran"
          title={captureError() || (screenContext() ? `Contexte joint : ${screenContext()!.app}${screenContext()!.window ? ` (${screenContext()!.window})` : ""}` : "Joindre le contexte visible à l’écran")}
          classList={{ active: !!screenContext(), error: !!captureError(), capturing: capturing() }}
          aria-pressed={!!screenContext()}
          disabled={capturing() || thinking()}
          onClick={async () => {
            setCapturing(true);
            setCaptureError("");
            try {
              const ctx = await captureVisibleScreen();
              if (ctx.available) setScreenContext(ctx);
            } catch (e: any) {
              setCaptureError(e?.message ?? String(e));
            } finally {
              setCapturing(false);
              setTimeout(() => inputEl?.focus(), 30);
            }
          }}>
          <Icon name={screenContext() ? "check" : "box-select"} size={19} />
        </button>
        <button title={expanded() ? "Réduire" : "Développer"} aria-label={expanded() ? "Réduire la barre" : "Développer la barre"} onClick={() => resize(!expanded())}>
          <Icon name={expanded() ? "chevron-down" : "chevron-up"} size={19} />
        </button>
      </div>
    </div>
  );
}

render(() => <Bar />, document.getElementById("root")!);
