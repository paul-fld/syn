// Barre « Demander à Syn » (maquette Accueil) : + | input | box-select · micro · ondes.
import { createSignal, type JSX } from "solid-js";
import { Icon } from "./Icon";
import { label } from "../lib/voice";
import { settings } from "../lib/state";
import type { ScreenContext } from "../lib/ipc";
import { captureVisibleScreen } from "../lib/screenContext";

export function AskBar(props: {
  onSubmit: (text: string, screenContext?: ScreenContext | null) => void;
  autofocus?: boolean;
  disabled?: boolean;
}): JSX.Element {
  const [text, setText] = createSignal("");
  const [screenContext, setScreenContext] = createSignal<ScreenContext | null>(null);
  const [capturing, setCapturing] = createSignal(false);
  const [captureError, setCaptureError] = createSignal("");
  const submit = () => {
    const t = text().trim();
    if (!t || props.disabled) return;
    setText("");
    const context = screenContext();
    setScreenContext(null);
    props.onSubmit(t, context);
  };
  return (
    <div class="askbar">
      <button title="Pièces jointes non disponibles dans cette version" disabled>
        <Icon name="plus" size={16} />
      </button>
      <input
        placeholder={captureError() ? "Capture impossible. Survole l’icône pour en savoir plus." : screenContext() ? "Contexte d’écran joint. Que veux-tu faire ?" : label("ask.placeholder", settings()?.voice)}
        value={text()}
        disabled={props.disabled}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
        ref={(el) => props.autofocus && setTimeout(() => el.focus(), 50)}
      />
      <button
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
      <button title="Dictée non disponible dans cette version" disabled>
        <Icon name="mic" size={15} />
      </button>
      <button title="Envoyer" onClick={submit}>
        <Icon name="audio-lines" size={15} />
      </button>
    </div>
  );
}
