// Apprendre à Syn : faits durables, proches, et file d'inconnus (demande groupée).
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc } from "../lib/ipc";

export function Apprendre(): JSX.Element {
  const [fact, setFact] = createSignal("");
  const [factSaved, setFactSaved] = createSignal(false);
  const [unknowns, { refetch: refetchUnknowns }] = createResource(() => ipc.unknownsPending());
  const [people, { refetch: refetchPeople }] = createResource(() => ipc.peopleList());

  const [pName, setPName] = createSignal("");
  const [pRel, setPRel] = createSignal("");
  const [pEmail, setPEmail] = createSignal("");
  const [pBday, setPBday] = createSignal("");

  const saveFact = async () => {
    const t = fact().trim();
    if (!t) return;
    await ipc.addFact(t);
    setFact("");
    setFactSaved(true);
    setTimeout(() => setFactSaved(false), 2500);
  };

  const addPerson = async () => {
    if (!pName().trim()) return;
    await ipc.peopleAdd({
      name: pName().trim(),
      relationship: pRel().trim() || undefined,
      email: pEmail().trim() || undefined,
      birthday: pBday().trim() || undefined,
    });
    setPName("");
    setPRel("");
    setPEmail("");
    setPBday("");
    refetchPeople();
  };

  return (
    <div class="page">
      <div class="page-title">Apprendre à Syn</div>
      <div class="page-sub">
        Ajoute les informations personnelles que Syn doit retenir. Elles restent chiffrées sur cet appareil.
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="brain" size={15} />
          Apprendre un fait durable
        </div>
        <div style={{ display: "flex", gap: "8px" }}>
          <input
            class="text-input"
            placeholder="Ex. : Mon checkup médical est mardi à 15h · Ma mère s'appelle Anne"
            value={fact()}
            onInput={(e) => setFact(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && saveFact()}
          />
          <button class="btn primary" onClick={saveFact}>
            Mémoriser
          </button>
        </div>
        <Show when={factSaved()}>
          <div class="sub" style={{ color: "var(--ok)", "margin-top": "8px" }}>
            Ajouté à tes connaissances.
          </div>
        </Show>
      </div>

      <Show when={(unknowns() ?? []).length > 0}>
        <div class="card">
          <div class="card-title">
            <Icon name="circle-fading-plus" size={15} />
            {(unknowns() ?? []).length} nom(s) à identifier
          </div>
          <div class="page-sub" style={{ "margin-bottom": "10px" }}>
            Indique leur relation avec toi ou ignore-les.
          </div>
          <For each={unknowns() ?? []}>
            {(u: any) => {
              const [rel, setRel] = createSignal("");
              return (
                <div class="row-line">
                  <span class="grow">
                    <b>{u.name}</b>
                    <span class="sub"> ({u.context})</span>
                  </span>
                  <input
                    class="text-input"
                    style={{ width: "150px" }}
                    placeholder="relation (ami, mère…)"
                    value={rel()}
                    onInput={(e) => setRel(e.currentTarget.value)}
                  />
                  <button
                    title="Enregistrer"
                    onClick={async () => {
                      await ipc.unknownLabel(u.id, u.name, rel() || undefined);
                      refetchUnknowns();
                      refetchPeople();
                    }}
                  >
                    <Icon name="check" size={15} />
                  </button>
                  <button
                    title="Ignorer"
                    onClick={async () => {
                      await ipc.unknownIgnore(u.id);
                      refetchUnknowns();
                    }}
                  >
                    <Icon name="x" size={15} />
                  </button>
                </div>
              );
            }}
          </For>
        </div>
      </Show>

      <div class="card">
        <div class="card-title">
          <Icon name="contact-round" size={15} />
          Ajouter un proche
        </div>
        <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
          <input class="text-input" style={{ flex: "2", "min-width": "140px" }} placeholder="Nom" value={pName()} onInput={(e) => setPName(e.currentTarget.value)} />
          <input class="text-input" style={{ flex: "1", "min-width": "110px" }} placeholder="Relation" value={pRel()} onInput={(e) => setPRel(e.currentTarget.value)} />
          <input class="text-input" style={{ flex: "2", "min-width": "150px" }} placeholder="Email (optionnel)" value={pEmail()} onInput={(e) => setPEmail(e.currentTarget.value)} />
          <input class="text-input" style={{ flex: "1", "min-width": "110px" }} placeholder="Anniv. MM-JJ" value={pBday()} onInput={(e) => setPBday(e.currentTarget.value)} />
          <button class="btn primary" onClick={addPerson}>
            Ajouter
          </button>
        </div>
      </div>

      <div class="section-label">Proches connus ({(people() ?? []).length})</div>
      <For each={people() ?? []}>
        {(p: any) => (
          <div class="row-line">
            <Icon name="circle-user-round" size={15} />
            <span class="grow">
              <b>{p.name}</b>
              <Show when={p.relationship}>
                <span class="sub"> ({p.relationship})</span>
              </Show>
            </span>
            <Show when={p.birthday}>
              <span class="sub">
                <Icon name="cake" size={12} /> {p.birthday}
              </span>
            </Show>
          </div>
        )}
      </For>
    </div>
  );
}
