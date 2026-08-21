// Connaissances : mémoire utile + vue compacte des sources indexées.
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc } from "../lib/ipc";
import { fmtDate, settings } from "../lib/state";
import { label } from "../lib/voice";

const SECTIONS = [
  { id: "overview", label: "Vue d’ensemble" },
  { id: "web", label: "Ta toile" },
  { id: "timeline", label: "Chronologie" },
  { id: "habits", label: "Habitudes" },
  { id: "files", label: "Fichiers" },
  { id: "mail", label: "Mails" },
  { id: "facts", label: "Mémoire" },
] as const;

type Section = (typeof SECTIONS)[number]["id"];

const RELATION_FR: Record<string, string> = {
  ecrit_a: "échange de messages",
  auteur_de: "a écrit",
  apparait_dans: "apparaît dans",
  co_destinataire: "en copie ensemble",
  reunit: "même rendez-vous",
  classe_dans: "rangé dans",
};

const ENTREE_ICONE: Record<string, string> = {
  mail_recu: "mail",
  mail_envoye: "mail-open",
  document: "file",
  rendez_vous: "calendar",
  engagement: "flag",
  action: "circle-check-big",
  conversation: "message-square",
};

export function Connaissances(): JSX.Element {
  const [section, setSection] = createSignal<Section>("overview");
  const [filter, setFilter] = createSignal("");
  const [stats, { refetch: refetchStats }] = createResource(() => ipc.knowledgeStats());
  const [fileGroups, { refetch: refetchGroups }] = createResource(() => ipc.knowledgeFileGroups());
  const [web, { refetch: refetchWeb }] = createResource(() => ipc.memoryGraph());
  const [habits, { refetch: refetchHabits }] = createResource(() => ipc.habitsList());
  const [focus, setFocus] = createSignal<string | null>(null);
  const [links] = createResource(focus, (nom: string) => ipc.memoryRelations(nom));
  const [timeline] = createResource(
    () => ({ section: section(), sujet: filter().trim() }),
    ({ section, sujet }) =>
      section === "timeline" ? ipc.memoryTimeline(30, sujet || null, 60) : Promise.resolve(null),
  );
  const [rebuilding, setRebuilding] = createSignal(false);

  const rebuild = async () => {
    setRebuilding(true);
    try {
      await ipc.memoryRebuild();
      await Promise.all([refetchWeb(), refetchHabits()]);
    } finally {
      setRebuilding(false);
    }
  };

  const decide = async (id: string, accepte: boolean) => {
    await ipc.habitsDecide(id, accepte);
    await refetchHabits();
  };

  const setIdentity = async (address: string, mine: boolean) => {
    await ipc.memorySetIdentity(address, mine);
    await refetchWeb();
  };
  const [items, { refetch }] = createResource(
    () => ({ section: section(), filter: filter().trim() }),
    ({ section, filter }) => {
      if (section === "mail") return ipc.listKnowledge("mail", filter || null, 200);
      if (section === "facts") return ipc.listKnowledge("conversation", filter || null, 200);
      if (section === "files") return ipc.listKnowledge("files", filter || null, 200);
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
              <Icon name="x" size={13} />
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
        <div class="chip"><Icon name="folder" size={13} /> {count("files")} {count("files") === 1 ? "fichier utile" : "fichiers utiles"}</div>
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
        <Show when={section() !== "overview" && section() !== "web" && section() !== "habits"}>
          <input
            class="text-input"
            placeholder={section() === "timeline" ? "Filtrer par personne, projet ou mot…" : "Rechercher dans cette section…"}
            value={filter()}
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
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
          <button class="card knowledge-summary" onClick={() => setSection("web")}>
            <Icon name="workflow" size={18} />
            <span><b>Ta toile</b><small>{web()?.stats?.relations ?? 0} liens entre tes proches, tes documents et tes rendez-vous</small></span>
            <Icon name="chevron-right" size={14} />
          </button>
        </div>
        <div class="empty-note knowledge-explanation">
          Syn indexe les fichiers personnels utiles à tes recherches. Les éléments techniques et
          les fichiers système sont ignorés.
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
                      <small>{group.count - (group.examples?.length ?? 0)} autres éléments. Utilise la recherche pour les retrouver.</small>
                    </Show>
                  </div>
                </details>
              )}
            </For>
          </div>
        }>{itemList()}</Show>
      </Show>

      <Show when={section() === "mail" || section() === "facts"}>{itemList()}</Show>

      {/* La toile : ce que Syn a relié, et ce qu'il n'a pas su trancher seul. */}
      <Show when={section() === "web"}>
        <div class="knowledge-stats">
          <div class="chip"><Icon name="workflow" size={13} /> {web()?.stats?.relations ?? 0} liens observés</div>
          <div class="chip"><Icon name="contact-round" size={13} /> {web()?.stats?.noeuds ?? 0} personnes et objets reliés</div>
          <div class="chip"><Icon name="mail" size={13} /> {web()?.stats?.contacts ?? 0} correspondants connus</div>
        </div>

        <Show when={(web()?.identites_retenues ?? []).length === 0 && (web()?.identites ?? []).length > 0}>
          <div class="card">
            <div class="card-title"><Icon name="info" size={15} /> Laquelle de ces adresses est la tienne ?</div>
            <div class="empty-note">
              Sans le savoir, Syn ne peut pas distinguer un message que tu as reçu d'un
              message que tu as envoyé — donc pas te signaler ce qui attend une réponse.
            </div>
            <For each={web()?.identites ?? []}>
              {(candidat: any) => (
                <div class="row-line">
                  <Icon name="mail" size={14} />
                  <span class="grow">
                    {candidat.address}
                    <span class="sub"> présente dans {candidat.presence_pct} % de tes messages</span>
                  </span>
                  <button class="btn" onClick={() => setIdentity(candidat.address, true)}>C'est moi</button>
                </div>
              )}
            </For>
          </div>
        </Show>

        <Show when={(web()?.identites_retenues ?? []).length > 0}>
          <div class="row-line">
            <Icon name="mail" size={14} />
            <span class="grow">
              Tes adresses
              <span class="sub"> {(web()?.identites_retenues ?? []).join(", ")}</span>
            </span>
            <button title="Ce n'est pas mon adresse" onClick={() => setIdentity((web()?.identites_retenues ?? [])[0], false)}>
              <Icon name="x" size={13} />
            </button>
          </div>
        </Show>

        <div class="section-label">Avec qui tu échanges le plus</div>
        <Show when={(web()?.correspondants ?? []).length === 0}>
          <div class="empty-note">
            La toile se tisse en arrière-plan à partir de tes messages et de ton agenda.
            Elle sera visible ici dès que Syn aura de la matière.
          </div>
        </Show>
        <For each={web()?.correspondants ?? []}>
          {(personne: any) => (
            <div class="row-line">
              <Icon name="contact-round" size={14} />
              <span class="grow">
                {personne.label}
                <span class="sub"> {personne.echanges} échanges · dernier {fmtDate(personne.last_seen)}</span>
              </span>
              <button class="btn" onClick={() => setFocus(personne.id)}>Voir ses liens</button>
            </div>
          )}
        </For>

        <Show when={focus()}>
          <div class="card">
            <div class="card-title">
              <Icon name="workflow" size={15} /> {links()?.noeud?.label ?? focus()}
            </div>
            <Show when={links()?.trouve === false}>
              <div class="empty-note">{links()?.note}</div>
            </Show>
            <For each={[
              { titre: "Documents et messages", cle: "documents_lies" },
              { titre: "Gens en commun", cle: "gens_en_commun" },
              { titre: "Rendez-vous", cle: "rendez_vous" },
            ]}>
              {(bloc) => (
                <Show when={(links()?.[bloc.cle] ?? []).length > 0}>
                  <div class="section-label">{bloc.titre}</div>
                  <For each={links()?.[bloc.cle] ?? []}>
                    {(lien: any) => (
                      <div class="row-line">
                        <Icon name="corner-down-right" size={14} />
                        <span class="grow">
                          {lien.label}
                          <span class="sub"> {RELATION_FR[lien.relation] ?? lien.relation} · vu {lien.observations} fois · {fmtDate(lien.last_seen)}</span>
                        </span>
                      </div>
                    )}
                  </For>
                </Show>
              )}
            </For>
          </div>
        </Show>

        <div class="row-line">
          <Icon name="repeat" size={14} />
          <span class="grow">
            Reconstruire la toile
            <span class="sub"> à faire après avoir corrigé ton adresse — rien n'est perdu, tout est relu depuis les sources</span>
          </span>
          <button class="btn" disabled={rebuilding()} onClick={rebuild}>
            {rebuilding() ? "En cours…" : "Reconstruire"}
          </button>
        </div>
      </Show>

      {/* Chronologie : ce qui s'est passé, et quand. */}
      <Show when={section() === "timeline"}>
        <Show when={(timeline()?.jours ?? []).length === 0}>
          <div class="empty-note">Rien à afficher sur les 30 derniers jours.</div>
        </Show>
        <For each={timeline()?.jours ?? []}>
          {(jour: any) => (
            <div class="card">
              <div class="card-title"><Icon name="calendar" size={15} /> {jour.jour}</div>
              <For each={jour.entrees ?? []}>
                {(entree: any) => (
                  <div class="row-line">
                    <Icon name={ENTREE_ICONE[entree.kind] ?? "hash"} size={14} />
                    <span class="grow">
                      {entree.title}
                      <span class="sub"> {entree.heure}{entree.detail ? ` · ${entree.detail}` : ""}</span>
                    </span>
                  </div>
                )}
              </For>
            </div>
          )}
        </For>
      </Show>

      {/* Habitudes : rien n'est appliqué sans que l'utilisateur l'ait validé. */}
      <Show when={section() === "habits"}>
        <div class="empty-note">
          Syn observe comment tu travailles. Rien n'est appliqué avant que tu l'aies confirmé,
          et ce que tu refuses ne revient pas.
        </div>
        <Show when={(habits() ?? []).length === 0}>
          <div class="empty-note">Aucune habitude observée pour l'instant.</div>
        </Show>
        <For each={habits() ?? []}>
          {(habitude: any) => (
            <div class="row-line">
              <Icon name={habitude.status === "confirmed" ? "circle-check-big" : "eye"} size={14} />
              <span class="grow">
                {habitude.phrase}
                <span class="sub">
                  {" "}{habitude.evidence} · observé {habitude.observations} fois
                  {habitude.status === "confirmed" ? " · confirmée par toi" : ""}
                </span>
              </span>
              <Show when={habitude.status !== "confirmed"}>
                <button class="btn" onClick={() => decide(habitude.id, true)}>C'est juste</button>
              </Show>
              <button title="Non, ce n'est pas mon habitude" onClick={() => decide(habitude.id, false)}>
                <Icon name="x" size={13} />
              </button>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}
