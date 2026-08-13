// Accueil (maquette App desktop) : salutation, Demander à Syn, brief explicable.
import { createResource, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { AskBar } from "../components/AskBar";
import { ActionCard } from "../components/ActionCard";
import { ipc, type BriefItem } from "../lib/ipc";
import { label } from "../lib/voice";
import { briefVersion, pendingActions, setBarQuery, setPage, settings } from "../lib/state";

const SERVICE_ICON: Record<string, string> = {
  message: "message",
  calendar: "calendrier",
  gmail: "gmail",
  mail: "apple-mail",
  clock: "clock",
  flag: "flag",
  gauge: "gauge",
};

function splitLines(text: string): [string, string] {
  // Coupe visuelle douce comme la maquette (1re ligne forte, suite grisée).
  if (text.length < 46) return [text, ""];
  const cut = text.lastIndexOf(" ", 46);
  return [text.slice(0, cut), text.slice(cut + 1)];
}

export function Accueil(): JSX.Element {
  const [brief] = createResource(briefVersion, () => ipc.getStartupBrief());

  const ask = (text: string) => {
    setBarQuery(text);
    setPage("conversations");
  };

  const openRef = (ref: string | null) => {
    if (ref) ipc.openSource(ref).catch(() => setPage("connaissances"));
  };

  return (
    <div class="page">
      <div class="home-wrap fade-in">
        <div class="home-greeting">
          {brief()?.greeting ?? label("greeting", settings()?.voice)}
        </div>

        <AskBar onSubmit={ask} autofocus />

        <Show when={pendingActions().length > 0}>
          <div style={{ "margin-top": "18px", display: "flex", "flex-direction": "column", gap: "10px" }}>
            <For each={pendingActions()}>{(a) => <ActionCard action={a} />}</For>
          </div>
        </Show>

        <Show when={brief()} keyed>
          {(b) => (
            <>
              <Show when={!b.empty} fallback={<div class="empty-note">{label("brief.empty", settings()?.voice)}</div>}>
                <div class="brief-list">
                  <For each={b.items}>
                    {(item: BriefItem) => {
                      const [l1, l2] = splitLines(item.text);
                      return (
                        <div class="brief-row fade-in">
                          <span class="lead">
                            <Icon name="corner-down-right" size={14} />
                          </span>
                          <span class="service">
                            <Icon name={SERVICE_ICON[item.icon] ?? "bell"} size={16} />
                          </span>
                          <span class="text">
                            <b>{l1}</b>
                            <Show when={l2}>
                              <br />
                              {l2}
                            </Show>
                            <Show when={item.sub}>
                              <br />
                              <span class="muted">{item.sub}</span>
                            </Show>
                          </span>
                          <span class="row-actions">
                            <Show when={item.kind === "mail"}>
                              <button title="Ouvrir la source" onClick={() => openRef(item.source_ref)}>
                                <Icon name="external-link" size={13} />
                              </button>
                            </Show>
                            <button
                              title="Demander à Syn de traiter"
                              onClick={() => ask(`À propos de : ${item.text} — aide-moi à traiter ça.`)}
                            >
                              <Icon name="square-pen" size={13} />
                            </button>
                          </span>
                        </div>
                      );
                    }}
                  </For>
                </div>

                <div class="chips">
                  <For each={b.chips}>
                    {(chip) => (
                      <button class="chip" onClick={() => (chip.source_ref ? openRef(chip.source_ref) : ask(chip.text))}>
                        <Icon name={chip.icon === "cake" ? "cake" : "file"} size={14} />
                        {chip.text}
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            </>
          )}
        </Show>
      </div>
    </div>
  );
}
