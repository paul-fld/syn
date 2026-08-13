// Connaissances : gérer ce que Syn a appris (stats, exploration, oubli).
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc } from "../lib/ipc";
import { fmtDate, settings } from "../lib/state";
import { label } from "../lib/voice";

const SOURCES = [
  { id: null, label: "Tout" },
  { id: "files", label: "Fichiers" },
  { id: "mail", label: "Mails" },
  { id: "conversation", label: "Faits appris" },
];

export function Connaissances(): JSX.Element {
  const [source, setSource] = createSignal<string | null>(null);
  const [filter, setFilter] = createSignal("");
  const [stats, { refetch: refetchStats }] = createResource(() => ipc.knowledgeStats());
  const [items, { refetch }] = createResource(
    () => ({ s: source(), f: filter() }),
    ({ s, f }) => ipc.listKnowledge(s, f || null, 200),
  );

  const total = () =>
    (stats()?.by_type ?? []).reduce((acc: number, x: any) => acc + (x.count ?? 0), 0);

  return (
    <div class="page">
      <div class="page-title">Connaissances</div>
      <div class="page-sub">{label("knowledge.sub", settings()?.voice)}</div>

      <div style={{ display: "flex", gap: "10px", "flex-wrap": "wrap", "margin-bottom": "16px" }}>
        <div class="chip">
          <Icon name="database" size={13} /> {total()} éléments
        </div>
        <div class="chip">
          <Icon name="brain" size={13} /> {stats()?.embeddings ?? 0} fragments compris
        </div>
        <div class="chip">
          <Icon name="contact-round" size={13} /> {stats()?.people ?? 0} personnes
        </div>
        <div class="chip">
          <Icon name="book" size={13} /> {stats()?.facts ?? 0} faits appris
        </div>
      </div>

      <div style={{ display: "flex", gap: "8px", "margin-bottom": "14px", "align-items": "center" }}>
        <For each={SOURCES}>
          {(s) => (
            <button
              class="btn"
              style={{ background: source() === s.id ? "var(--bg-selected)" : "transparent" }}
              onClick={() => setSource(s.id)}
            >
              {s.label}
            </button>
          )}
        </For>
        <input
          class="text-input"
          style={{ "max-width": "260px", "margin-left": "auto" }}
          placeholder="Filtrer par titre ou chemin…"
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
        />
      </div>

      <Show when={(items() ?? []).length === 0}>
        <div class="empty-note">
          Rien ici pour l'instant. Ajoute des dossiers à indexer dans Connecteurs.
        </div>
      </Show>
      <For each={items() ?? []}>
        {(it: any) => (
          <div class="row-line">
            <Icon
              name={
                it.type === "photo" ? "file" : it.type === "email" ? "mail" : it.type === "fact" ? "brain" : it.type === "code_project" ? "folder" : "file"
              }
              size={14}
            />
            <span class="grow" title={it.source_ref}>
              {it.title || it.source_ref}
              <span class="sub"> · {it.type} · {fmtDate(it.ingested_at)}</span>
            </span>
            <Show when={it.source === "files" || (it.source === "mail" && it.path)}>
              <button title="Ouvrir la source" onClick={() => ipc.openSource(it.source_ref).catch(() => {})}>
                <Icon name="external-link" size={13} />
              </button>
            </Show>
            <button
              title="Faire oublier à Syn"
              onClick={async () => {
                await ipc.forgetItem(it.id);
                refetch();
                refetchStats();
              }}
            >
              <Icon name="circle-x" size={13} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
