// Barre « Demander à Syn » (maquette Accueil) : + | input | box-select · micro · ondes.
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Icon } from "./Icon";
import { activeSession, attachDocument, conversation, detachDocument } from "../lib/conversations";
import { label } from "../lib/voice";
import { settings, refreshSettings } from "../lib/state";
import { ipc, type ScreenContext } from "../lib/ipc";
import { captureVisibleScreen } from "../lib/screenContext";

/// Deux gestes vocaux distincts, comme sur la maquette :
/// — micro : DICTER, le texte remplit le champ et l'utilisateur relit avant d'envoyer ;
/// — ondes : COMMANDE VOCALE, la demande part dès que l'on cesse de parler.
/// La différence n'est pas cosmétique : dicter laisse une relecture, commander non.
type VoiceMode = "dictation" | "command";

export function AskBar(props: {
  onSubmit: (text: string, screenContext?: ScreenContext | null) => void;
  autofocus?: boolean;
  disabled?: boolean;
}): JSX.Element {
  const [text, setText] = createSignal("");
  const [screenContext, setScreenContext] = createSignal<ScreenContext | null>(null);
  const [capturing, setCapturing] = createSignal(false);
  const [captureError, setCaptureError] = createSignal("");
  const [voiceMode, setVoiceMode] = createSignal<VoiceMode | null>(null);
  const [joining, setJoining] = createSignal(false);
  const [joinError, setJoinError] = createSignal("");
  const documents = () => conversation(activeSession()).documents;
  const [voiceError, setVoiceError] = createSignal("");
  let poller: number | undefined;
  let inputEl: HTMLInputElement | undefined;

  const submit = (value?: string) => {
    const t = (value ?? text()).trim();
    if (!t || props.disabled) return;
    setText("");
    const context = screenContext();
    setScreenContext(null);
    props.onSubmit(t, context);
  };

  const stopPolling = () => {
    if (poller !== undefined) {
      clearInterval(poller);
      poller = undefined;
    }
  };

  /// Arrête l'écoute et rend la transcription finale.
  const stopVoice = async (send: boolean) => {
    stopPolling();
    const mode = voiceMode();
    setVoiceMode(null);
    const finalText = await ipc.dictationStop().catch(() => "");
    const spoken = (finalText || text()).trim();
    if (!spoken) return;
    if (send && mode === "command") submit(spoken);
    else {
      setText(spoken);
      inputEl?.focus();
    }
  };

  const startVoice = async (mode: VoiceMode) => {
    setVoiceError("");
    if (voiceMode()) {
      await stopVoice(true);
      return;
    }
    const status = await ipc.dictationStatus().catch(() => null);
    if (!status?.supported) {
      setVoiceError("La dictée n'est disponible que sur macOS.");
      return;
    }
    if (!settings()?.voice_input_enabled) {
      setVoiceError("Active la dictée dans Réglages ▸ Général.");
      return;
    }
    // macOS ne rend l'autorisation qu'après un aller-retour système : on la
    // demande, puis on laisse l'utilisateur relancer une fois qu'il a répondu.
    if (status.authorization !== "granted") {
      await ipc.dictationRequestPermission().catch(() => null);
      setVoiceError("Autorise le micro et la reconnaissance vocale, puis réessaie.");
      return;
    }
    try {
      await ipc.dictationStart();
    } catch (e: any) {
      setVoiceError(e?.message ?? String(e));
      return;
    }
    setVoiceMode(mode);
    setText("");
    // La transcription arrive par morceaux : on la reflète dans le champ pour
    // que l'utilisateur VOIE ce qui est compris pendant qu'il parle.
    poller = window.setInterval(async () => {
      const snapshot = await ipc.dictationTranscript().catch(() => null);
      if (!snapshot) return;
      setText(snapshot.text);
      if (!snapshot.listening) void stopVoice(true);
    }, 250);
  };

  onCleanup(() => {
    stopPolling();
    if (voiceMode()) void ipc.dictationStop().catch(() => {});
  });

  const listening = (mode: VoiceMode) => voiceMode() === mode;
  const placeholder = () => {
    if (voiceMode() === "command") return "Parle : ta demande partira toute seule…";
    if (voiceMode() === "dictation") return "Je t'écoute…";
    if (captureError()) return "Capture impossible. Survole l’icône pour en savoir plus.";
    if (screenContext()) return "Contexte d’écran joint. Que veux-tu faire ?";
    return label("ask.placeholder", settings()?.voice);
  };

  return (
    <>
    <Show when={documents().length > 0 || joinError()}>
      <div class="askbar-documents">
        <For each={documents()}>
          {(document) => (
            <span class="document-chip" title={`${document.path}${document.truncated ? " · lu en partie" : ""}`}>
              <Icon name="file" size={13} />
              {document.name}
              <Show when={document.truncated}><em>· lu en partie</em></Show>
              <button
                class="document-chip-remove"
                aria-label={`Retirer ${document.name}`}
                onClick={() => {
                  const session = activeSession();
                  if (session) void detachDocument(session, document.id);
                }}
              >
                <Icon name="x" size={11} />
              </button>
            </span>
          )}
        </For>
        <Show when={joinError()}><span class="document-chip error">{joinError()}</span></Show>
      </div>
    </Show>
    <div class="askbar" classList={{ listening: !!voiceMode() }}>
      {/* Confier un document à la conversation. Syn le lit une fois : son
          contenu entre ensuite dans chaque tour, sans dépendre d'une recherche. */}
      <button
        title="Joindre un document à la conversation"
        aria-label="Joindre un document"
        disabled={props.disabled || joining()}
        onClick={async () => {
          setJoining(true);
          try {
            const chosen = await openDialog({ multiple: true, title: "Documents à confier à Syn" });
            const paths = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
            for (const path of paths) {
              await attachDocument(activeSession(), path);
            }
          } catch (error: any) {
            setJoinError(error?.message ?? String(error));
          } finally {
            setJoining(false);
          }
        }}
      >
        <Icon name="plus" size={16} />
      </button>
      <input
        aria-label="Demander à Syn"
        placeholder={placeholder()}
        value={text()}
        disabled={props.disabled}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
        ref={(el) => {
          inputEl = el;
          if (props.autofocus) setTimeout(() => el.focus(), 50);
        }}
      />
      <button
        aria-label="Joindre le contexte visible à l’écran"
        title={captureError() || (screenContext() ? `Contexte joint : ${screenContext()!.app}${screenContext()!.window ? ` (${screenContext()!.window})` : ""}` : "Joindre le contexte visible à l’écran")}
        classList={{ active: !!screenContext(), error: !!captureError(), capturing: capturing() }}
        aria-pressed={!!screenContext()}
        disabled={capturing() || props.disabled}
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
        }
      }}>
        <Icon name={screenContext() ? "check" : "box-select"} size={15} />
      </button>
      <button
        aria-label={listening("dictation") ? "Arrêter la dictée" : "Dicter la demande"}
        title={voiceError() || (listening("dictation") ? "Arrêter la dictée" : "Dicter : le texte remplit le champ, tu relis avant d’envoyer")}
        classList={{ active: listening("dictation"), error: !!voiceError() }}
        aria-pressed={listening("dictation")}
        disabled={props.disabled}
        onClick={() => {
          void refreshSettings();
          void startVoice("dictation");
        }}
      >
        <Icon name="mic" size={15} />
      </button>
      <button
        aria-label={listening("command") ? "Arrêter la commande vocale" : "Commande vocale"}
        title={voiceError() || (listening("command") ? "Arrêter et envoyer" : "Commande vocale : ta demande part dès que tu as fini de parler")}
        classList={{ active: listening("command"), error: !!voiceError() }}
        aria-pressed={listening("command")}
        disabled={props.disabled}
        onClick={() => {
          void refreshSettings();
          void startVoice("command");
        }}
      >
        <Icon name="audio-lines" size={15} />
      </button>
    </div>
    </>
  );
}
