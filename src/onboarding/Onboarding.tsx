// Onboarding (maquettes Steps 1-3 + mise en route) : panneau vague à gauche,
// contenu à droite, points de progression. Chaque étape est skippable et
// rejouable ; permissions et modèle d'abord — rien ne marche sans eux.
import { createResource, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Icon } from "../components/Icon";
import { Logo } from "../components/Logo";
import onboardingBackground from "../assets/Background-image.png";
import { ipc, on, type HardwareProfile } from "../lib/ipc";
import { refreshStatus, status } from "../lib/state";

export function Onboarding(): JSX.Element {
  // Si la base existe déjà (rejouer l'onboarding), on saute l'étape 1.
  // ?ob_step=N : prévisualisation directe d'une étape (mode démo).
  const forced = Number(new URLSearchParams(location.search).get("ob_step")) || 0;
  const [step, setStep] = createSignal(
    forced || (status()?.initialized && status()?.unlocked ? 2 : 1),
  );

  return (
    <div class="onboard-shell">
      <div
        class="onboard-left"
        style={{ "background-image": `url(${onboardingBackground})` }}
      >
        <div class="onboard-logo">
          <Logo size={92} />
        </div>
      </div>
      <div class="onboard-right">
        <Show when={step() === 1}>
          <Step1 next={() => setStep(2)} />
        </Show>
        <Show when={step() === 2}>
          <Step2 next={() => setStep(3)} />
        </Show>
        <Show when={step() === 3}>
          <Step3 next={() => setStep(4)} />
        </Show>
        <Show when={step() === 4}>
          <Step4 />
        </Show>
        <div class="onboard-dots">
          {[1, 2, 3, 4].map((i) => (
            <span class="dot" classList={{ active: step() === i }} />
          ))}
        </div>
      </div>
    </div>
  );
}

// — Étape 1 : Bienvenue + mot de passe maître (+ phrase de récupération) —
function Step1(props: { next: () => void }): JSX.Element {
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [showPw, setShowPw] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [phrase, setPhrase] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const submit = async () => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      const r = await ipc.setupMaster(email().trim() || null, password());
      setPhrase(r.recovery_phrase);
      await refreshStatus();
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="onboard-panel fade-in">
      <Show
        when={!phrase()}
        fallback={
          <>
            <div class="onboard-title">Votre phrase de récupération</div>
            <div class="recovery-box">{phrase()}</div>
            <div class="onboard-note">
              Conservez cette phrase hors ligne. Elle permet de récupérer vos données si vous
              oubliez votre mot de passe.
            </div>
            <div class="onboard-actions" style={{ "justify-content": "center" }}>
              <button class="btn primary" onClick={props.next}>
                Je l'ai notée
              </button>
            </div>
          </>
        }
      >
        <div class="onboard-title">Bienvenue sur Syn</div>
        <div class="onboard-input">
          <input
            placeholder="Email"
            value={email()}
            onInput={(e) => setEmail(e.currentTarget.value)}
          />
        </div>
        <div class="onboard-input">
          <input
            type={showPw() ? "text" : "password"}
            placeholder="Mot de passe"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
          <button onClick={() => setShowPw(!showPw())}>
            <Icon name={showPw() ? "eye-off" : "eye"} size={18} />
          </button>
        </div>
        <Show when={error()}>
          <div class="sub" style={{ color: "var(--danger)", "text-align": "center" }}>
            {error()}
          </div>
        </Show>
        <div class="onboard-actions" style={{ "justify-content": "center" }}>
          <button class="btn primary" disabled={busy()} onClick={submit}>
            {busy() ? "Création…" : "Créer mon espace local"}
          </button>
        </div>
        <div class="onboard-note">
          Ce mot de passe protège les données enregistrées sur cet appareil.
        </div>
      </Show>
    </div>
  );
}

// — Étape 2 : Contacts & proches (maquette Step 2) —
function Step2(props: { next: () => void }): JSX.Element {
  const [contacts, setContacts] = createSignal<any>(null);
  const importContacts = async () => {
    try {
      setContacts({ ok: true as const, list: await ipc.peopleOsPreview() });
    } catch (e: any) {
      setContacts({ ok: false as const, error: e?.message ?? String(e), list: [] });
    }
  };
  const [added, setAdded] = createSignal<Set<string>>(new Set());
  const [manualName, setManualName] = createSignal("");
  const [manualPhone, setManualPhone] = createSignal("");

  const add = async (name: string, phone?: string, email?: string) => {
    await ipc.peopleAdd({ name, phone, email });
    setAdded((s) => new Set(s).add(name));
  };

  return (
    <div class="onboard-panel fade-in">
      <div class="onboard-title">
        Ajoutez vos contact et identifiez
        <br />
        vos proches
      </div>

      <Show when={!contacts()}>
        <button class="btn" onClick={importContacts}>
          Importer depuis Contacts macOS…
        </button>
        <div class="onboard-note">Syn ne lit pas le carnet d’adresses avant ce consentement explicite.</div>
      </Show>

      <Show when={contacts()?.ok && (contacts()?.list ?? []).length > 0}>
        <div class="onboard-list">
          <For each={(contacts()?.list ?? []).slice(0, 40)}>
            {(c: any) => (
              <div class="onboard-list-row">
                <span class="grow">
                  {c.name}
                  {c.phone ? ` : ${c.phone}` : c.email ? ` : ${c.email}` : ""}
                </span>
                <Show
                  when={!added().has(c.name)}
                  fallback={<Icon name="check" size={18} />}
                >
                  <button class="add-btn" onClick={() => add(c.name, c.phone, c.email)}>
                    <Icon name="plus" size={18} />
                  </button>
                </Show>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={contacts() && !contacts()!.ok}>
        <div class="onboard-note" style={{ "margin-bottom": "10px" }}>
          {(contacts() as any).error}
        </div>
      </Show>

      <div style={{ display: "flex", gap: "8px", "margin-top": "14px" }}>
        <div class="onboard-input" style={{ flex: "2", "margin-bottom": "0" }}>
          <input placeholder="Nom (ex. Maman)" value={manualName()} onInput={(e) => setManualName(e.currentTarget.value)} />
        </div>
        <div class="onboard-input" style={{ flex: "2", "margin-bottom": "0" }}>
          <input placeholder="Téléphone ou email" value={manualPhone()} onInput={(e) => setManualPhone(e.currentTarget.value)} />
        </div>
        <button
          class="btn"
          style={{ "align-self": "center" }}
          onClick={async () => {
            if (!manualName().trim()) return;
            const contact = manualPhone().trim();
            await add(manualName().trim(), contact.includes("@") ? undefined : contact, contact.includes("@") ? contact : undefined);
            setManualName("");
            setManualPhone("");
          }}
        >
          <Icon name="plus" size={16} />
        </button>
      </div>

      <div class="onboard-actions">
        <button class="link-btn" onClick={props.next}>
          Passer
        </button>
        <button class="btn primary" onClick={props.next}>
          Continuer
        </button>
      </div>
    </div>
  );
}

// — Étape 3 : Connecter les services (maquette Step 3) —
const SERVICES = [
  { id: "apple", label: "Apple", icon: "apple" },
  { id: "microsoft", label: "Microsoft", icon: "microsoft" },
  { id: "google", label: "Google", icon: "google" },
  { id: "slack", label: "Slack", icon: "slack" },
  { id: "github", label: "Github", icon: "github" },
];

function Step3(props: { next: () => void }): JSX.Element {
  const nativeService = navigator.userAgent.includes("Mac") ? "apple" : navigator.userAgent.includes("Windows") ? "microsoft" : null;
  const [messages, setMessages] = createSignal<Record<string, string>>({});
  const [connected, setConnected] = createSignal<Set<string>>(new Set(nativeService ? [nativeService] : []));

  const connect = async (id: string) => {
    try {
      const r = await ipc.connectorConnect(id);
      if (r?.status === "connected") {
        setConnected((s) => new Set(s).add(id));
      }
      if (r?.message) setMessages((m) => ({ ...m, [id]: r.message }));
    } catch (e: any) {
      setMessages((m) => ({ ...m, [id]: e?.message ?? String(e) }));
    }
  };

  return (
    <div class="onboard-panel fade-in">
      <div class="onboard-title">Connectez vos services</div>
      <div class="onboard-list">
        <For each={SERVICES}>
          {(s) => (
            <>
              <div class="onboard-list-row">
                <span class="service">
                  <Icon name={s.icon} size={22} />
                </span>
                <span class="grow">{s.label}{s.id === nativeService ? " (sur cet appareil)" : ""}</span>
                <Show when={!connected().has(s.id)} fallback={<Icon name="check" size={18} />}>
                  <button class="add-btn" onClick={() => connect(s.id)}>
                    <Icon name="plus" size={18} />
                  </button>
                </Show>
              </div>
              <Show when={messages()[s.id]}>
                <div class="onboard-note" style={{ "text-align": "left", padding: "6px 18px", margin: "0" }}>
                  {messages()[s.id]}
                </div>
              </Show>
            </>
          )}
        </For>
      </div>
      <div class="onboard-actions">
        <button class="link-btn" onClick={props.next}>
          Passer
        </button>
        <button class="btn primary" onClick={props.next}>
          Continuer
        </button>
      </div>
      <div class="onboard-note">
        Vous pourrez modifier chaque connexion plus tard.
      </div>
    </div>
  );
}

// — Étape 4 : Mise en route (matériel → modèle → dossiers → confidentialité → index) —
function Step4(): JSX.Element {
  const [hw] = createResource<HardwareProfile>(() => ipc.hardwareInfo());
  const [llm, { refetch: refetchLlm }] = createResource(() => ipc.llmStatus());
  const [pull, setPull] = createSignal<Record<string, { pct: number; status: string }>>({});
  const [folders, setFolders] = createSignal<string[]>([]);
  const [indexing, setIndexing] = createSignal<{ done: number; total: number } | null>(null);
  const [finishing, setFinishing] = createSignal(false);

  onMount(async () => {
    const profile = await ipc.hardwareInfo();
    await ipc.setSettings({
      tier: profile.tier,
      chat_model: profile.chat_model,
      embed_model: profile.embed_model,
    });
    refetchLlm();
    const un1 = await on("model_pull_progress", (p) => {
      setPull((cur) => ({ ...cur, [p.model]: { pct: p.pct, status: p.status } }));
      if (p.pct >= 100) refetchLlm();
    });
    const un2 = await on("ingestion_status", (p) => {
      setIndexing({ done: p.done, total: p.total });
    });
    onCleanup(() => {
      un1();
      un2();
    });
  });

  const needsChat = () => llm() && !llm()!.chat_model_ready;
  const needsEmbed = () => llm() && !llm()!.embed_model_ready;

  const download = (model: string) => {
    setPull((cur) => ({ ...cur, [model]: { pct: 0, status: "démarrage…" } }));
    ipc.modelPull(model);
  };

  const addFolder = async () => {
    const dir = await openDialog({ directory: true, multiple: false, title: "Dossier à indexer" });
    if (typeof dir === "string") {
      await ipc.filesAddFolder(dir);
      setFolders((f) => [...f, dir]);
    }
  };

  const finish = async () => {
    setFinishing(true);
    await ipc.onboardingComplete();
    await refreshStatus();
  };

  return (
    <div class="onboard-panel fade-in" style={{ "max-height": "78vh", "overflow-y": "auto" }}>
      <div class="onboard-title">Mise en route sur votre machine</div>

      <Show when={hw()} keyed>
        {(h) => (
          <div class="onboard-note" style={{ "text-align": "left", "margin-bottom": "12px" }}>
            Syn a choisi les modèles adaptés à cette machine ({h.total_ram_gb} Go de mémoire,
            {h.cpu_count} cœurs).
          </div>
        )}
      </Show>

      <Show when={llm() && !llm()!.available}>
        <div class="rule-feedback" style={{ "margin-bottom": "12px" }}>
          Le moteur local Ollama est indisponible. Démarrez-le pour activer les réponses de Syn.
        </div>
      </Show>

      <Show when={llm()?.available}>
        <For each={[
          { model: hw()?.chat_model ?? "llama3.1:latest", label: "Modèle de conversation", missing: needsChat() },
          { model: hw()?.embed_model ?? "nomic-embed-text", label: "Recherche locale", missing: needsEmbed() },
        ]}>
          {(m) => (
            <div class="row-line" style={{ "flex-wrap": "wrap" }}>
              <Icon name="cloud-download" size={14} />
              <span class="grow">
                {m.label} <span class="sub">{m.model}</span>
              </span>
              <Show
                when={m.missing}
                fallback={<span class="pill-status ok">prêt</span>}
              >
                <Show
                  when={pull()[m.model]}
                  fallback={
                    <button class="btn" onClick={() => download(m.model)}>
                      Télécharger
                    </button>
                  }
                >
                  <span class="sub" style={{ "min-width": "160px" }}>
                    {pull()[m.model].pct >= 0 ? `${Math.round(pull()[m.model].pct)} % ` : ""}
                    {pull()[m.model].status}
                  </span>
                </Show>
              </Show>
            </div>
          )}
        </For>
      </Show>

      <div class="section-label">Dossiers à indexer</div>
      <For each={folders()}>
        {(f) => (
          <div class="row-line">
            <Icon name="folder" size={14} />
            <span class="grow">{f}</span>
          </div>
        )}
      </For>
      <button class="btn" onClick={addFolder} style={{ "align-self": "flex-start" }}>
        <Icon name="folder-input" size={14} /> Choisir un dossier…
      </button>
      <Show when={indexing()}>
        <div class="sub" style={{ "margin-top": "10px" }}>
          Analyse des fichiers : {indexing()!.done}/{indexing()!.total}
          <div class="progress-track">
            <div
              class="progress-fill"
              style={{ width: `${(indexing()!.done / Math.max(1, indexing()!.total)) * 100}%` }}
            />
          </div>
        </div>
      </Show>

      <div class="onboard-note" style={{ "text-align": "left" }}>
        Vos données restent chiffrées sur cet appareil. Vous pourrez les exporter ou les supprimer
        depuis Réglages, puis Données.
      </div>

      <div class="onboard-actions">
        <span />
        <button class="btn primary" disabled={finishing()} onClick={finish}>
          Ouvrir Syn
        </button>
      </div>
    </div>
  );
}
