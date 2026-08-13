// Connecteurs : accès global aux fichiers sous contrôle macOS + services Apple
// locaux et OAuth externes, permissions explicites et révocables.
import { createResource, createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ipc, on, type ConnectorInfo, type IndexStatus, type NativePermission } from "../lib/ipc";

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
  const fileAccess = () => native()?.services.find((service) => service.id === "files");

  const refreshIndex = () => ipc.filesIndexStatus().then(setIndexStatus).catch(() => {});
  const refreshPermissions = async () => {
    const result = await refetchNative();
    const files = result?.services?.find((service: NativePermission) => service.id === "files");
    if (files?.status === "granted") {
      const activation = await ipc.filesActivateFullAccess().catch(() => null);
      if (activation?.started) setMessage("Accès vérifié. Syn indexe maintenant automatiquement tes fichiers personnels.");
    }
  };
  onMount(() => {
    refreshIndex();
    void refreshPermissions();
    const t = setInterval(() => {
      refreshIndex();
      void refreshPermissions();
      void refetch();
    }, 3000);
    onCleanup(() => clearInterval(t));
    on("sync_progress", (p) => {
      if (p?.payload?.message || p?.message) setMessage(p.message ?? p?.payload?.message);
    });
  });

  const connect = async (id: string) => {
    try {
      const r = await ipc.connectorConnect(id);
      if (r?.authorization_url) await openUrl(String(r.authorization_url));
      if (r?.message) setMessage(r.message);
      refetch();
    } catch (error: any) {
      setMessage(error?.message ?? String(error));
    }
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
          Accès aux fichiers
          <span class="spacer" />
          <Show when={fileAccess()?.status === "granted"}>
            <span class="pill-status ok">Autorisé</span>
          </Show>
        </div>
        <div class="sub" style={{ "margin-bottom": "12px", "line-height": "1.5" }}>
          Une seule autorisation permet à Syn de retrouver automatiquement les fichiers de ton compte.
          Les fichiers système, caches, dépendances, applications et formats techniques sont ignorés.
        </div>
        <Show when={fileAccess()?.status !== "granted"} fallback={
          <div style={{ display: "flex", gap: "8px", "align-items": "center" }}>
            <button class="btn" onClick={() => ipc.filesReindex()}>Réindexer maintenant</button>
            <span class="sub">Surveillance automatique active</span>
          </div>
        }>
          <button class="btn primary" onClick={async () => {
            const result = await ipc.filesRequestFullAccess();
            setMessage(result.message);
            await refreshPermissions();
          }}>Autoriser l’accès aux fichiers</button>
          <div class="sub" style={{ "margin-top": "8px" }}>
            macOS ouvre Confidentialité et sécurité → Accès complet au disque. Active Syn puis reviens dans l’application.
          </div>
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
        <For each={(native()?.services ?? []).filter((service) => service.id !== "files")}>
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
              <Show when={permission.operational && ((permission.id === "mail" && permission.status !== "granted") || permission.status === "denied" || permission.status === "restricted")}>
                <button class="btn" onClick={async () => { await ipc.openNativeSettings(permission.settings); setMessage("Modifie l’autorisation de Syn dans Réglages système, puis reviens ici."); }}>Ouvrir Réglages</button>
              </Show>
              <Show when={permission.operational && permission.id !== "mail" && permission.status === "needs_permission"}>
                <button class="btn primary" onClick={async () => {
                  const result = await ipc.requestNativePermission(permission.id);
                  setMessage(result.status === "granted" || result.status === "limited" ? `${permission.label} autorisé.` : `${permission.label} n’a pas été autorisé.`);
                  refetchNative();
                }}>Autoriser</button>
              </Show>
              <Show when={permission.operational && permission.id === "mail" && permission.status === "granted"}>
                <button class="btn" onClick={() => connect("apple")}>Synchroniser</button>
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
