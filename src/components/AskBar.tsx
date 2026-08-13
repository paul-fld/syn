// Barre « Demander à Syn » (maquette Accueil) : + | input | box-select · micro · ondes.
import { createSignal, type JSX } from "solid-js";
import { Icon } from "./Icon";
import { label } from "../lib/voice";
import { settings } from "../lib/state";
import { ipc } from "../lib/ipc";

export function AskBar(props: {
  onSubmit: (text: string) => void;
  autofocus?: boolean;
  disabled?: boolean;
}): JSX.Element {
  const [text, setText] = createSignal("");
  const submit = () => {
    const t = text().trim();
    if (!t || props.disabled) return;
    setText("");
    props.onSubmit(t);
  };
  return (
    <div class="askbar">
      <button title="Pièces jointes non disponibles dans cette version" disabled>
        <Icon name="plus" size={16} />
      </button>
      <input
        placeholder={label("ask.placeholder", settings()?.voice)}
        value={text()}
        disabled={props.disabled}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
        ref={(el) => props.autofocus && setTimeout(() => el.focus(), 50)}
      />
      <button title="Contexte d'écran" onClick={async () => {
        try {
          const ctx = await ipc.screenContext();
          if (ctx?.available) setText(`À propos de ${ctx.app}${ctx.window ? ` — ${ctx.window}` : ""} : `);
        } catch {}
      }}>
        <Icon name="box-select" size={15} />
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
