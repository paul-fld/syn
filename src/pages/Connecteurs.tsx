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
  syncing: { label: "Connecté · cache en cours", cls: "working" },
  disconnected: { label: "Non connecté", cls: "" },
  needs_reauth: { label: "Reconnexion requise", cls: "err" },
  needs_permission: { label: "Permission requise", cls: "warn" },
  needs_configuration: { label: "Configuration requise", cls: "warn" },
  authorized_only: { label: "Connecté", cls: "ready" },
  unavailable: { label: "Indisponible", cls: "err" },
};

const NATIVE_DESCRIPTION: Record<string, string> = {
  mail: "Accède aux messages enregistrés dans Mail.",
  contacts: "Retrouve les personnes enregistrées dans Contacts.",
  calendar: "Consulte et modifie les événements du Calendrier.",
  reminders: "Synchronise les rappels ouverts avec les tâches et les briefs de Syn.",
  photos: "Recherche dans Photos bientôt disponible.",
  screen: "Analyse ponctuellement la fenêtre visible à ta demande.",
};

const EXTERNAL_DESCRIPTION: Record<string, string> = {
  google: "Gmail, Google Agenda et Google Drive.",
  microsoft: "Outlook, Calendrier et OneDrive.",
  slack: "Messages et espaces de travail Slack.",
  github: "Dépôts, problèmes et demandes de fusion GitHub.",
};

export function Connecteurs(): JSX.Element {
  const [connectors, { refetch }] = createResource(() => ipc.connectorStatus());
  const [native, { refetch: refetchNative }] = createResource(() => ipc.nativePermissions());
  const [indexStatus, setIndexStatus] = createSignal<IndexStatus | null>(null);
  const [message, setMessage] = createSignal<string | null>(null);
  const [pendingAuth, setPendingAuth] = createSignal<string | null>(null);
  const [syncProgress, setSyncProgress] = createSignal<Record<string, { pct: number; message: string }>>({});
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
    let polling = false;
    let pollCount = 0;
    const poll = async () => {
      if (polling) return;
      polling = true;
      try {
        await refreshIndex();
        pollCount += 1;
        // Le statut d'index reste fluide à 5 s. Le statut OAuth (qui consulte
        // aussi le trousseau macOS) n'est sondé rapidement que pendant une
        // connexion ; sinon 30 s suffisent et évitent les micro-blocages.
        if (pendingAuth() || pollCount % 6 === 0) {
          await Promise.resolve(refetch()).then((list: ConnectorInfo[] | null | undefined) => {
          const waiting = pendingAuth();
          const current = list?.find((connector) => connector.id === waiting);
          if (current && (current.status === "authorized_only" || current.status === "syncing" || current.status === "connected" || current.last_error)) {
            setPendingAuth(null);
          }
          });
        }
      } finally {
        polling = false;
      }
    };
    // Les autorisations natives ne changent pas spontanément pendant que la
    // page est ouverte. Éviter leur resondage supprime des appels macOS et une
    // activation de l'indexeur toutes les trois secondes.
    const t = setInterval(() => void poll(), 5000);
    onCleanup(() => clearInterval(t));
    // L'écouteur doit être désabonné au démontage : il s'accumulait à chaque
    // visite de la page (audit §3).
    let unlisten: (() => void) | null = null;
    on("sync_progress", (raw) => {
      const p = raw?.payload ?? raw;
      if (p?.message) setMessage(p.message);
      if (p?.connector) {
        setSyncProgress((current) => ({
          ...current,
          [p.connector]: {
            pct: Math.max(current[p.connector]?.pct ?? 0, Number(p.pct ?? 0)),
            message: String(p.message ?? ""),
          },
        }));
      }
    }).then((un) => (unlisten = un));
    onCleanup(() => unlisten?.());
  });

  const connect = async (id: string) => {
    try {
      setPendingAuth(id);
      const r = await ipc.connectorConnect(id);
      if (r?.authorization_url) await openUrl(String(r.authorization_url));
      if (r?.message) setMessage(r.message);
      refetch();
    } catch (error: any) {
      setPendingAuth(null);
      setMessage(error?.message ?? String(error));
    }
  };

  const sync = async (id: string) => {
    try {
      setSyncProgress((current) => ({ ...current, [id]: { pct: 1, message: "Préparation de la synchronisation…" } }));
      setMessage(`Synchronisation ${id} en cours…`);
      const request = ipc.connectorSync(id);
      await new Promise((resolve) => setTimeout(resolve, 150));
      await refetch();
      const result = await request;
      setMessage(result.preparing
        ? `${id === "google" ? "Google" : "Microsoft"} est accessible immédiatement. Le cache complet se prépare en arrière-plan.`
        : `${id === "google" ? "Google" : "Microsoft"} synchronisé : ${result.mail} mail(s), ${result.files} fichier(s), ${result.events} événement(s).`);
      await refetch();
    } catch (error: any) {
      setMessage(error?.message ?? String(error));
      await refetch();
    }
  };

  const formatLastSync = (value: number | null) => {
    if (!value) return null;
    return new Intl.DateTimeFormat("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }).format(new Date(value * 1000));
  };
  const friendlyError = (error: string | null) => {
    if (!error) return "";
    if (/401|invalid_grant|expired|réautorisation/i.test(error)) return "La session a expiré ou a été révoquée. Reconnecte le compte pour continuer.";
    if (/403|insufficient|permission|scope/i.test(error)) return "Certaines autorisations nécessaires n’ont pas été accordées. Reconnecte le compte et accepte tous les accès demandés.";
    if (/redirect_uri/i.test(error)) return "L’adresse de retour OAuth ne correspond pas à celle configurée chez le fournisseur.";
    if (/client_secret.*missing/i.test(error)) return "Ce client Google exige son secret OAuth. Ajoute SYN_GOOGLE_CLIENT_SECRET dans .env, redémarre Syn, puis reconnecte le compte.";
    if (/invalid_client/i.test(error)) return "Le client OAuth configuré n’est pas accepté par le fournisseur.";
    if (/network|réseau|dns|connect/i.test(error)) return "Le service est temporairement inaccessible. Vérifie la connexion puis réessaie.";
    return error.length > 240 ? `${error.slice(0, 237)}…` : error;
  };

  const services = () => (connectors() ?? []).filter((c) => BRAND_ICON[c.id] && c.id !== "apple");

  return (
    <div class="page">
      <div class="page-title">Connecteurs</div>
      <div class="page-sub">
        Gère les services et les données accessibles à Syn.
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
          Une autorisation suffit pour retrouver tes fichiers personnels. Syn ignore les fichiers
          système, les applications et les caches.
        </div>
        <Show when={fileAccess()?.status !== "granted"} fallback={
          <div style={{ display: "flex", gap: "8px", "align-items": "center" }}>
            <Icon name="check" size={14} />
            <span class="sub">Index local maintenu automatiquement — aucune réindexation manuelle nécessaire</span>
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
        <Show when={indexStatus()?.running && indexStatus()?.phase === "cataloging"}>
          <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>
            Préparation rapide du catalogue : {indexStatus()!.done}/{indexStatus()!.total}
            <div class="progress-track">
              <div
                class="progress-fill"
                style={{ width: `${(indexStatus()!.done / Math.max(1, indexStatus()!.total)) * 100}%` }}
              />
            </div>
            <span class="mono muted">{indexStatus()!.current}</span>
          </div>
        </Show>
        <Show when={indexStatus()?.catalog_ready}>
          <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>
            <Icon name="check" size={13} /> Fichiers disponibles immédiatement
            <Show when={indexStatus()?.phase === "enriching"}> · enrichissement de tout le corpus en arrière-plan</Show>
          </div>
          <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>
            Couverture sémantique : {indexStatus()!.coverage_pct.toFixed(1)} %
            ({indexStatus()!.embedded_count}/{indexStatus()!.eligible_count} éléments éligibles)
            <div class="progress-track">
              <div class="progress-fill" style={{ width: `${Math.min(100, indexStatus()!.coverage_pct)}%` }} />
            </div>
            <span class="muted">
              Index lexical : {indexStatus()!.lexical_count}/{indexStatus()!.eligible_count} · reprise FSEvents : {indexStatus()!.replayed_events} changement(s)
              <Show when={(indexStatus()?.fallback_count ?? 0) > 0}> · {indexStatus()!.fallback_count} repli(s) catalogue</Show>
            </span>
          </div>
        </Show>
        <For each={indexStatus()?.cloud_bootstraps ?? []}>
          {(bootstrap) => {
            const service = bootstrap.provider === "google"
              ? (bootstrap.resource === "gmail" ? "Gmail" : "Google Drive")
              : (bootstrap.resource === "mail" ? "Outlook" : "OneDrive");
            const denominator = bootstrap.total ?? bootstrap.processed;
            const pct = bootstrap.total
              ? Math.min(100, bootstrap.processed * 100 / Math.max(1, bootstrap.total))
              : null;
            return (
              <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>
                Catalogue {service} disponible progressivement : {bootstrap.processed}
                {bootstrap.total != null ? `/${denominator}` : " éléments"}
                <Show when={pct != null}>
                  <div class="progress-track">
                    <div class="progress-fill" style={{ width: `${pct}%` }} />
                  </div>
                </Show>
                <span class="muted">La recherche directe reste disponible pendant cette préparation.</span>
              </div>
            );
          }}
        </For>
        <Show when={(indexStatus()?.pending_embeddings ?? 0) > 0}>
          <div class="sub muted" style={{ "margin-top": "6px" }}>
            {indexStatus()!.pending_embeddings} passages seront analysés lorsque le moteur local sera disponible.
          </div>
        </Show>
        <Show when={(indexStatus()?.sensitive_skipped ?? 0) > 0}>
          <div class="sub muted" style={{ "margin-top": "6px" }}>
            {indexStatus()!.sensitive_skipped} document(s) sensibles ignorés. Modifie ce choix dans
            Réglages, puis Confidentialité.
          </div>
        </Show>
        <Show when={(indexStatus()?.unreadable_files ?? 0) > 0}>
          <div class="sub muted" style={{ "margin-top": "6px" }}>
            {indexStatus()!.unreadable_files} document(s) n'ont pas de texte extractible ; ils restent recherchables par nom et chemin.
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
          Ces services utilisent les autorisations de macOS. Aucun compte supplémentaire n'est nécessaire.
        </div>
        <For each={(native()?.services ?? []).filter((service) => service.id !== "files")}>
          {(permission: NativePermission) => (
            <div class="row-line native-permission-row">
              <Icon name={permission.id === "mail" ? "apple-mail" : permission.id === "calendar" ? "calendrier" : permission.id === "files" ? "folder" : permission.id === "screen" ? "app-window-mac" : permission.id === "contacts" ? "contact-round" : permission.id === "photos" ? "camera" : "check"} size={15} />
              <span class="grow">
                <b>{permission.label}</b>
                <span class="sub"> {NATIVE_DESCRIPTION[permission.id] ?? permission.detail}</span>
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
        <Show when={(connectors() ?? []).some((c) => c.id === "messages")}>
          <div class="row-line native-permission-row">
            <Icon name="message" size={15} />
            <span class="grow">
              <b>Messages</b>
              <span class="sub"> Historique iMessage/SMS lu localement, rattaché à tes proches.</span>
            </span>
            <span class={`pill-status ${(connectors() ?? []).find((c) => c.id === "messages")?.status === "connected" ? "ok" : ""}`}>
              {(connectors() ?? []).find((c) => c.id === "messages")?.status === "connected"
                ? "Synchronisé"
                : "Via l'accès complet au disque"}
            </span>
          </div>
        </Show>
      </div>

      <div class="section-label">Services externes</div>
      <div class="external-connectors">
      <For each={services().filter((connector) => connector.id === "google" || connector.id === "microsoft")}>
        {(c: ConnectorInfo) => (
          <div class={`external-connector-card state-${c.status}`}>
            <span class="external-connector-icon">
              <Icon name={BRAND_ICON[c.id]} size={20} />
            </span>
            <div class="external-connector-content">
              <div class="external-connector-heading">
                <strong>{c.id === "google" ? "Google Workspace" : "Microsoft 365"}</strong>
                <span class={`connector-status ${STATUS_LABEL[c.status]?.cls ?? ""}`}>
                  <i />{STATUS_LABEL[c.status]?.label ?? c.status}
                </span>
              </div>
              <p>{EXTERNAL_DESCRIPTION[c.id]}</p>
              <Show when={c.status === "connected" && c.sync_summary}>
                <div class="connector-meta"><Icon name="check" size={12} /> {c.sync_summary}<Show when={formatLastSync(c.last_sync)}> · dernière mise à jour {formatLastSync(c.last_sync)}</Show></div>
              </Show>
              <Show when={c.status === "authorized_only"}>
                <div class="connector-notice">Le compte est accessible. Syn prépare automatiquement son cache local en arrière-plan.</div>
              </Show>
              <Show when={c.last_error}>
                <div class="connector-error"><Icon name="circle-alert" size={13} /><span>{friendlyError(c.last_error)}</span></div>
              </Show>
              <Show when={c.status === "syncing"}>
                <div class="connector-progress">
                  <div><span>{syncProgress()[c.id]?.message || "Synchronisation en cours…"}</span><b>{Math.round(syncProgress()[c.id]?.pct ?? 0)} %</b></div>
                  <div class="progress-track"><div class="progress-fill" style={{ width: `${syncProgress()[c.id]?.pct ?? 4}%` }} /></div>
                </div>
              </Show>
            </div>
            <div class="external-connector-actions">
              <Show when={c.status === "disconnected" || c.status === "needs_reauth"}>
                <button class="btn primary connector-cta" disabled={pendingAuth() === c.id} onClick={() => connect(c.id)}>
                  {pendingAuth() === c.id ? "Autorisation en cours…" : c.status === "needs_reauth" ? "Reconnecter" : "Connecter"}
                </button>
              </Show>
              <Show when={c.status === "authorized_only"}>
                <button class="btn connector-secondary" onClick={() => sync(c.id)}>Actualiser le cache</button>
              </Show>
              <Show when={c.status === "connected"}>
                <button class="btn connector-secondary" onClick={() => sync(c.id)}>Actualiser</button>
              </Show>
              <Show when={c.status === "syncing"}>
                <button class="btn connector-secondary" disabled>Synchronisation…</button>
              </Show>
              <Show when={c.status === "connected" || c.status === "authorized_only"}>
                <button class="connector-disconnect" onClick={() => ipc.connectorDisconnect(c.id).then(() => refetch())}>Déconnecter</button>
              </Show>
            </div>
          </div>
        )}
      </For>
      </div>

      <div class="section-label">Autres intégrations</div>
      <div class="upcoming-connectors">
        <For each={services().filter((connector) => connector.id !== "google" && connector.id !== "microsoft")}>
          {(c) => <div><span class="external-connector-icon"><Icon name={BRAND_ICON[c.id]} size={18} /></span><span><b style={{ "text-transform": "capitalize" }}>{c.id}</b><small>{EXTERNAL_DESCRIPTION[c.id]}</small></span><em>Bientôt disponible</em></div>}
        </For>
      </div>

    </div>
  );
}
