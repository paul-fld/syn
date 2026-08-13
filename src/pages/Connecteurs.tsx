// Connecteurs : dossiers indexés (moindre privilège) + services (Apple local,
// OAuth globaux honnêtement statués), permissions explicites et révocables.
import { createResource, createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Icon } from "../components/Icon";
import { ipc, on, type ConnectorInfo, type IndexStatus, type NativePermission } from "../lib/ipc";
import { fmtDate } from "../lib/state";

const BRAND_ICON: Record<string, string> = {
  apple: "apple",
  google: "google",
  microsoft: "microsoft",
  slack: "slack",
  github: "github",
};

const STATUS_LABEL: Record<string, { label: string; cls: string }> = {
  connected: { label: "Connecté", cls: "ok" },
  syncing: { label: "Synchronisation…", cls: "warn" },
  disconnected: { label: "Non connecté", cls: "" },
  needs_reauth: { label: "À ré-autoriser", cls: "warn" },
  needs_permission: { label: "Permission requise", cls: "warn" },
  needs_configuration: { label: "Configuration requise", cls: "warn" },
  unavailable: { label: "Indisponible", cls: "err" },
};

export function Connecteurs(): JSX.Element {
  const [connectors, { refetch }] = createResource(() => ipc.connectorStatus());
  const [native, { refetch: refetchNative }] = createResource(() => ipc.nativePermissions());
  const [indexStatus, setIndexStatus] = createSignal<IndexStatus | null>(null);
  const [message, setMessage] = createSignal<string | null>(null);

  const refreshIndex = () => ipc.filesIndexStatus().then(setIndexStatus).catch(() => {});
  onMount(() => {
    refreshIndex();
    const t = setInterval(refreshIndex, 3000);
    onCleanup(() => clearInterval(t));
    on("sync_progress", (p) => {
      if (p?.payload?.message || p?.message) setMessage(p.message ?? p?.payload?.message);
    });
  });

  const addFolder = async () => {
    const dir = await openDialog({ directory: true, multiple: false, title: "Choisir un dossier à indexer" });
    if (typeof dir === "string") {
      await ipc.filesAddFolder(dir);
      refreshIndex();
    }
  };

  const connect = async (id: string) => {
    const r = await ipc.connectorConnect(id);
    if (r?.message) setMessage(r.message);
    refetch();
  };

  const services = () => (connectors() ?? []).filter((c) => BRAND_ICON[c.id] && c.id !== "apple");

  return (
    <div class="page">
      <div class="page-title">Connecteurs</div>
      <div class="page-sub">
        Chaque connecteur est une permission explicite, révocable, et tracée dans le journal
        d'accès. Syn n'ouvre aucune connexion sortante en dehors de ceux que tu actives.
      </div>

      <Show when={message()}>
        <div class="rule-feedback fade-in" style={{ "margin-bottom": "14px" }}>
          {message()}
        </div>
      </Show>

      <div class="card">
        <div class="card-title">
          <Icon name="folder-open" size={15} />
          Dossiers indexés
          <span class="spacer" />
          <button class="btn" onClick={() => ipc.filesReindex()}>
            Tout réindexer
          </button>
          <button class="btn primary" onClick={addFolder}>
            Ajouter un dossier
          </button>
        </div>
        <Show
          when={(indexStatus()?.folders ?? []).length > 0}
          fallback={<div class="muted">Aucun dossier — Syn n'indexe que ce que tu lui confies.</div>}
        >
          <For each={indexStatus()?.folders ?? []}>
            {(f) => (
              <div class="row-line">
                <Icon name="folder" size={14} />
                <span class="grow" title={f.path}>
                  {f.path}
                  <span class="sub"> · indexé {f.last_indexed ? fmtDate(f.last_indexed) : "jamais"}</span>
                </span>
                <button title="Réindexer" onClick={() => ipc.filesReindex(f.path)}>
                  <Icon name="repeat" size={13} />
                </button>
                <button
                  title="Retirer du périmètre"
                  onClick={async () => {
                    await ipc.filesRemoveFolder(f.path);
                    refreshIndex();
                  }}
                >
                  <Icon name="circle-x" size={13} />
                </button>
              </div>
            )}
          </For>
        </Show>
        <Show when={indexStatus()?.running}>
          <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>
            Indexation en cours : {indexStatus()!.done}/{indexStatus()!.total}
            <div class="progress-track">
              <div
                class="progress-fill"
                style={{ width: `${(indexStatus()!.done / Math.max(1, indexStatus()!.total)) * 100}%` }}
              />
            </div>
            <span class="mono muted">{indexStatus()!.current}</span>
          </div>
        </Show>
        <Show when={(indexStatus()?.pending_embeddings ?? 0) > 0}>
          <div class="sub muted" style={{ "margin-top": "6px" }}>
            {indexStatus()!.pending_embeddings} fragments en attente d'embedding (moteur local
            indisponible — rattrapage automatique).
          </div>
        </Show>
        <Show when={(indexStatus()?.sensitive_skipped ?? 0) > 0}>
          <div class="sub muted" style={{ "margin-top": "6px" }}>
            {indexStatus()!.sensitive_skipped} document(s) sensibles non lus (santé, finance, ID) —
            autorise la lecture dans Réglages → Confidentialité.
          </div>
        </Show>
      </div>

      <div class="section-label">Services intégrés à cet appareil</div>
      <div class="card">
        <div class="card-title">
          <Icon name={native()?.platform === "macos" ? "apple" : "app-window"} size={19} />
          Services {native()?.provider ?? "du système"}
          <span class="pill-status ok">Intégré</span>
        </div>
        <div class="sub" style={{ "margin-bottom": "8px" }}>
          Aucun compte système à connecter : tu accordes ou révoques chaque autorisation séparément.
        </div>
        <For each={native()?.services ?? []}>
          {(permission: NativePermission) => (
            <div class="row-line native-permission-row">
              <Icon name={permission.id === "mail" ? "apple-mail" : permission.id === "calendar" ? "calendrier" : permission.id === "files" ? "folder" : permission.id === "screen" ? "app-window-mac" : permission.id === "contacts" ? "contact-round" : permission.id === "photos" ? "image" : "check"} size={15} />
              <span class="grow">
                <b>{permission.label}</b>
                <span class="sub"> — {permission.detail}</span>
              </span>
              <span class={`pill-status ${permission.status === "granted" || permission.status === "limited" ? "ok" : permission.status === "denied" || permission.status === "restricted" ? "err" : ""}`}>
                {!permission.operational ? "Intégration à finaliser" : permission.status === "granted" ? "Autorisé" : permission.status === "limited" ? "Accès limité" : permission.status === "needs_selection" ? "À sélectionner" : permission.status === "denied" ? "Refusé" : permission.status === "restricted" ? "Restreint" : permission.status === "unavailable" ? "Indisponible" : "À autoriser"}
              </span>
              <Show when={permission.id === "files"} fallback={
                <Show when={(permission.id === "mail" && permission.status !== "granted") || permission.status === "denied" || permission.status === "restricted"} fallback={
                  <button class="btn" disabled={!permission.operational || (permission.status === "granted" && permission.id !== "mail") || permission.status === "unavailable"} onClick={async () => {
                    if (permission.id === "mail") {
                      await connect("apple");
                      return;
                    }
                    const result = await ipc.requestNativePermission(permission.id);
                    setMessage(result.status === "granted" || result.status === "limited" ? `${permission.label} autorisé.` : `${permission.label} n’a pas été autorisé.`);
                    refetchNative();
                  }}>{!permission.operational ? "À intégrer" : permission.id === "mail" ? "Synchroniser" : permission.status === "granted" ? "Autorisé" : "Autoriser"}</button>
                }>
                  <button class="btn" onClick={async () => { await ipc.openNativeSettings(permission.settings); setMessage("Modifie l’autorisation de Syn dans Réglages système, puis reviens ici."); }}>Ouvrir Réglages</button>
                </Show>
              }>
                <button class="btn" onClick={async () => { await addFolder(); refetchNative(); }}>Choisir…</button>
              </Show>
            </div>
          )}
        </For>
      </div>

      <div class="section-label">Services externes</div>
      <For each={services()}>
        {(c: ConnectorInfo) => (
          <div class="row-line" style={{ padding: "11px 12px" }}>
            <span class="service">
              <Icon name={BRAND_ICON[c.id]} size={20} />
            </span>
            <span class="grow">
              <b style={{ "text-transform": "capitalize" }}>{c.id}</b>
              <Show when={c.detail}>
                <span class="sub"> — {c.detail}</span>
              </Show>
            </span>
            <span class={`pill-status ${STATUS_LABEL[c.status]?.cls ?? ""}`}>
              {STATUS_LABEL[c.status]?.label ?? c.status}
            </span>
            <Show
              when={c.status === "connected"}
              fallback={
                <button class="btn" disabled={c.status === "needs_configuration" || c.status === "syncing"} onClick={() => connect(c.id)}>
                  {c.status === "needs_configuration" ? "Non disponible" : "Connecter"}
                </button>
              }
            >
              <button class="btn" onClick={() => ipc.connectorDisconnect(c.id).then(() => refetch())}>
                Déconnecter
              </button>
            </Show>
          </div>
        )}
      </For>

    </div>
  );
}
