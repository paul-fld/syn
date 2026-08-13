// Connaissances : mémoire utile + vue compacte des sources indexées.
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc } from "../lib/ipc";
import { fmtDate, settings } from "../lib/state";
import { label } from "../lib/voice";

const SECTIONS = [
  { id: "overview", label: "Vue d’ensemble" },
  { id: "files", label: "Fichiers" },
  { id: "mail", label: "Mails" },
  { id: "facts", label: "Mémoire" },
] as const;

type Section = (typeof SECTIONS)[number]["id"];

export function Connaissances(): JSX.Element {
  const [section, setSection] = createSignal<Section>("overview");
  const [filter, setFilter] = createSignal("");
  const [stats, { refetch: refetchStats }] = createResource(() => ipc.knowledgeStats());
  const [fileGroups, { refetch: refetchGroups }] = createResource(() => ipc.knowledgeFileGroups());
  const [items, { refetch }] = createResource(
    () => ({ section: section(), filter: filter().trim() }),
    ({ section, filter }) => {
      if (section === "mail") return ipc.listKnowledge("mail", filter || null, 200);
      if (section === "facts") return ipc.listKnowledge("conversation", filter || null, 200);
      if (section === "files" && filter) return ipc.listKnowledge("files", filter, 200);
      return Promise.resolve([]);
    },
  );

  const count = (source?: string, type?: string) =>
    (stats()?.by_type ?? [])
      .filter((row: any) => (!source || row.source === source) && (!type || row.type === type))
      .reduce((total: number, row: any) => total + Number(row.count ?? 0), 0);

  const forget = async (id: string) => {
    await ipc.forgetItem(id);
    await Promise.all([refetch(), refetchStats(), refetchGroups()]);
  };

  const itemList = () => (
    <>
      <Show when={(items() ?? []).length === 0}>
        <div class="empty-note">Aucun élément correspondant.</div>
      </Show>
      <For each={items() ?? []}>
        {(item: any) => (
          <div class="row-line">
            <Icon name={item.type === "email" ? "mail" : item.type === "fact" ? "brain" : item.type === "code_project" ? "folder" : "file"} size={14} />
            <span class="grow" title={item.source_ref}>
              {item.title || item.source_ref}
              <span class="sub"> · {item.type === "fact" ? "fait mémorisé" : item.type} · {fmtDate(item.ingested_at)}</span>
            </span>
            <Show when={item.source === "files" || (item.source === "mail" && item.path)}>
              <button title="Ouvrir la source" onClick={() => ipc.openSource(item.source_ref).catch(() => {})}>
                <Icon name="external-link" size={13} />
              </button>
            </Show>
            <button title="Faire oublier à Syn" onClick={() => forget(item.id)}>
              <Icon name="circle-x" size={13} />
            </button>
          </div>
        )}
      </For>
    </>
  );

  return (
    <div class="page">
      <div class="page-title">Connaissances</div>
      <div class="page-sub">{label("knowledge.sub", settings()?.voice)}</div>

      <div class="knowledge-stats">
        <div class="chip"><Icon name="folder" size={13} /> {count("files")} fichiers utiles</div>
        <div class="chip"><Icon name="brain" size={13} /> {stats()?.embeddings ?? 0} passages indexés</div>
        <div class="chip"><Icon name="contact-round" size={13} /> {stats()?.people ?? 0} {stats()?.people === 1 ? "personne" : "personnes"}</div>
        <div class="chip"><Icon name="book" size={13} /> {stats()?.facts ?? 0} {stats()?.facts === 1 ? "fait appris" : "faits appris"}</div>
      </div>

      <div class="knowledge-toolbar">
        <For each={SECTIONS}>
          {(entry) => (
            <button class="btn" classList={{ active: section() === entry.id }} onClick={() => { setSection(entry.id); setFilter(""); }}>
              {entry.label}
            </button>
          )}
        </For>
        <Show when={section() !== "overview"}>
          <input class="text-input" placeholder="Rechercher dans cette section…" value={filter()} onInput={(event) => setFilter(event.currentTarget.value)} />
        </Show>
      </div>

      <Show when={section() === "overview"}>
        <div class="knowledge-overview-grid">
          <button class="card knowledge-summary" onClick={() => setSection("files")}>
            <Icon name="folder-open" size={18} />
            <span><b>Fichiers de ce Mac</b><small>{count("files")} éléments recherchables, rangés par emplacement</small></span>
            <Icon name="chevron-right" size={14} />
          </button>
          <button class="card knowledge-summary" onClick={() => setSection("mail")}>
            <Icon name="mail" size={18} />
            <span><b>Mails connectés</b><small>{count("mail")} messages accessibles à Syn</small></span>
            <Icon name="chevron-right" size={14} />
          </button>
          <button class="card knowledge-summary" onClick={() => setSection("facts")}>
            <Icon name="brain" size={18} />
            <span><b>Mémoire personnelle</b><small>Faits explicitement appris, personnes et préférences</small></span>
            <Icon name="chevron-right" size={14} />
          </button>
        </div>
        <div class="empty-note knowledge-explanation">
          Syn connaît les fichiers utiles pour les retrouver et les utiliser à ta demande. Les caches,
          dépendances, applications, bases techniques et fichiers système ne sont ni affichés ni indexés.
        </div>
      </Show>

      <Show when={section() === "files"}>
        <Show when={filter()} fallback={
          <div class="knowledge-groups">
            <For each={fileGroups() ?? []}>
              {(group: any) => (
                <details class="knowledge-group">
                  <summary>
                    <Icon name={group.name === "Projets" ? "briefcase" : "folder"} size={15} />
                    <span class="grow"><b>{group.name}</b><small>{group.count} éléments · actualisé {fmtDate(group.latest)}</small></span>
                    <Icon name="chevron-down" size={13} />
                  </summary>
                  <div class="knowledge-examples">
                    <For each={group.examples ?? []}>
                      {(example: any) => (
                        <button title={example.path} onClick={() => example.path && ipc.openSource(example.path).catch(() => {})}>
                          <Icon name={example.type === "code_project" ? "folder" : "file"} size={12} />
                          <span>{example.title || example.path}</span>
                        </button>
                      )}
                    </For>
                    <Show when={group.count > (group.examples?.length ?? 0)}>
                      <small>Et {group.count - (group.examples?.length ?? 0)} autres — utilise la recherche pour retrouver un fichier précis.</small>
                    </Show>
                  </div>
                </details>
              )}
            </For>
          </div>
        }>{itemList()}</Show>
      </Show>

      <Show when={section() === "mail" || section() === "facts"}>{itemList()}</Show>
    </div>
  );
}
