// Onglets Données, Stockage, Appareil de mon enfant [V2], Sécurité, Confidentialité, Aide.
import { createResource, createSignal, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { Toggle, SettingRow } from "../components/Toggle";
import { ipc } from "../lib/ipc";
import { settings, refreshSettings, status, refreshStatus, fmtBytes, setScreen } from "../lib/state";

const patch = async (p: Record<string, unknown>) => {
  await ipc.setSettings(p);
  refreshSettings();
};

export function TabDonnees(): JSX.Element {
  const [stats] = createResource(() => ipc.storageStats());
  const [purgePw, setPurgePw] = createSignal("");
  const [purgeArmed, setPurgeArmed] = createSignal(false);
  const [msg, setMsg] = createSignal<string | null>(null);

  return (
    <div>
      <div class="settings-h1">Données</div>
      <div class="card">
        <div class="card-title">
          <Icon name="database" size={15} /> Données locales
        </div>
        <div class="settings-card-value">
          {stats()?.items ?? "…"} éléments, {stats()?.embeddings ?? "…"} fragments, {stats()
            ? fmtBytes(stats()!.db_bytes)
            : "…"}
        </div>
        <div class="muted settings-path">
          <span class="mono">{stats()?.data_dir}</span>
        </div>
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="download" size={15} /> Exporter les données
        </div>
        <div class="muted settings-card-copy">
          Ouvre le dossier contenant la base chiffrée et ses métadonnées.
        </div>
        <button
          class="btn"
          onClick={async () => {
            await ipc.openSource(await ipc.dataDirPath()).catch(() => {});
          }}
        >
          Ouvrir le dossier
        </button>
      </div>

      <div class="card">
        <div class="card-title" style={{ color: "var(--danger)" }}>
          <Icon name="ban" size={15} /> Tout supprimer
        </div>
        <div class="muted settings-card-copy">
          Efface définitivement les données, l'index et les clés de cet appareil.
        </div>
        <Show
          when={purgeArmed()}
          fallback={
            <button class="btn danger" onClick={() => setPurgeArmed(true)}>
              Supprimer mes données…
            </button>
          }
        >
          <div style={{ display: "flex", gap: "8px" }}>
            <input
              class="text-input"
              type="password"
              placeholder="Mot de passe maître pour confirmer"
              value={purgePw()}
              onInput={(e) => setPurgePw(e.currentTarget.value)}
            />
            <button
              class="btn danger"
              onClick={async () => {
                try {
                  await ipc.purgeAllData(purgePw());
                  await refreshStatus();
                  setScreen("onboarding");
                } catch (e: any) {
                  setMsg(e?.message ?? String(e));
                }
              }}
            >
              Tout supprimer
            </button>
            <button class="btn" onClick={() => setPurgeArmed(false)}>
              Annuler
            </button>
          </div>
          <Show when={msg()}>
            <div class="sub" style={{ color: "var(--danger)", "margin-top": "8px" }}>
              {msg()}
            </div>
          </Show>
        </Show>
      </div>
    </div>
  );
}

export function TabStockage(): JSX.Element {
  const [stats] = createResource(() => ipc.storageStats());
  const [llm] = createResource(() => ipc.llmStatus());
  const [index] = createResource(() => ipc.filesIndexStatus());
  return (
    <div>
      <div class="settings-h1">Stockage</div>
      <SettingRow label="Base locale" desc="Index chiffré, reconstruit à partir de tes sources.">
        <span class="pill-status">{stats() ? fmtBytes(stats()!.db_bytes) : "…"}</span>
      </SettingRow>
      <SettingRow label="Fichiers indexés" desc="Gère les accès depuis la page Connecteurs.">
        <span class="pill-status">{index()?.folders.length ?? 0} dossiers</span>
      </SettingRow>
      <div class="section-label">Modèles locaux</div>
      <Show when={!llm()?.available}>
        <div class="rule-feedback">
          Le moteur local est indisponible. Les réponses générées sont momentanément désactivées.
        </div>
      </Show>
      <For each={llm()?.installed_models ?? []}>
        {(m) => (
          <div class="row-line">
            <Icon name="package" size={14} />
            <span class="grow">{m}</span>
            <Show when={m.startsWith(settings()?.chat_model.split(":")[0] ?? "")}>
              <span class="pill-status ok">Actif</span>
            </Show>
          </div>
        )}
      </For>
    </div>
  );
}

export function TabEnfant(): JSX.Element {
  return (
    <div>
      <div class="settings-h1">Appareil de mon enfant</div>
      <div class="card" style={{ opacity: 0.75 }}>
        <div class="card-title">
          <Icon name="baby" size={15} /> Indisponible
        </div>
        <div class="muted">
          Cette fonctionnalité sera proposée dans une prochaine version.
        </div>
      </div>
    </div>
  );
}

export function TabSecurite(): JSX.Element {
  const [cur, setCur] = createSignal("");
  const [neu, setNeu] = createSignal("");
  const [pwMsg, setPwMsg] = createSignal<string | null>(null);
  const [rpPw, setRpPw] = createSignal("");
  const [phrase, setPhrase] = createSignal<string | null>(null);
  return (
    <div>
      <div class="settings-h1">Sécurité</div>

      <div class="card">
        <div class="card-title">
          <Icon name="key-round" size={15} /> Mot de passe maître
        </div>
        <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
          <input
            class="text-input"
            style={{ flex: "1", "min-width": "160px" }}
            type="password"
            placeholder="Actuel"
            value={cur()}
            onInput={(e) => setCur(e.currentTarget.value)}
          />
          <input
            class="text-input"
            style={{ flex: "1", "min-width": "160px" }}
            type="password"
            placeholder="Nouveau (8 caractères minimum)"
            value={neu()}
            onInput={(e) => setNeu(e.currentTarget.value)}
          />
          <button
            class="btn primary"
            onClick={async () => {
              try {
                await ipc.changeMasterPassword(cur(), neu());
                setPwMsg("Mot de passe changé.");
                setCur("");
                setNeu("");
              } catch (e: any) {
                setPwMsg(e?.message ?? String(e));
              }
            }}
          >
            Changer
          </button>
        </div>
        <Show when={pwMsg()}>
          <div class="sub" style={{ "margin-top": "8px", color: "var(--text-secondary)" }}>{pwMsg()}</div>
        </Show>
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="file-lock-2" size={15} /> Phrase de récupération
        </div>
        <div class="muted settings-card-copy">
          Cette phrase permet de retrouver l'accès à tes données. La régénérer désactive
          l'ancienne.
        </div>
        <Show
          when={phrase()}
          fallback={
            <div style={{ display: "flex", gap: "8px" }}>
              <input
                class="text-input"
                type="password"
                placeholder="Mot de passe maître"
                value={rpPw()}
                onInput={(e) => setRpPw(e.currentTarget.value)}
              />
              <button
                class="btn"
                onClick={async () => {
                  try {
                    setPhrase(await ipc.regenerateRecovery(rpPw()));
                    setRpPw("");
                  } catch (e: any) {
                    setPhrase(null);
                    alert(e?.message ?? e);
                  }
                }}
              >
                Régénérer
              </button>
            </div>
          }
        >
          <div class="recovery-box">{phrase()}</div>
          <div class="sub muted" style={{ "margin-top": "8px" }}>
            Note-la hors-ligne, puis ferme cet écran.
          </div>
        </Show>
      </div>

      <SettingRow
        label="Trousseau du système"
        desc="Déverrouille Syn avec ta session macOS."
      >
        <Toggle
          checked={status()?.keychain ?? false}
          onChange={async (v) => {
            await ipc.setKeychain(v);
            refreshStatus();
          }}
        />
      </SettingRow>
    </div>
  );
}

export function TabConfidentialite(): JSX.Element {
  return (
    <div>
      <div class="settings-h1">Confidentialité</div>

      <SettingRow
        label="Traitement cloud"
        desc="Indisponible pour le moment. Toutes les demandes restent locales."
      >
        <Toggle
          checked={false}
          disabled
          onChange={() => {}}
        />
      </SettingRow>

      <SettingRow
        label="Contenu des fichiers"
        desc="Syn peut lire les fichiers autorisés. Leur contenu reste local et chiffré."
      >
        <span class="pill-status ok">Inclus</span>
      </SettingRow>

      <SettingRow label="Suggestions proactives" desc="Nombre maximal de suggestions par jour.">
        <select
          class="select"
          value={String(settings()?.rarity_budget ?? 5)}
          onChange={(e) => patch({ rarity_budget: Number(e.currentTarget.value) })}
        >
          {[2, 3, 5, 8, 12].map((n) => (
            <option value={String(n)}>{n} / jour</option>
          ))}
        </select>
      </SettingRow>

      <SettingRow
        label="Alerte de stockage"
        desc="Prévient lorsque l'espace libre passe sous ce seuil."
      >
        <select
          class="select"
          value={String(settings()?.guardian_disk_pct ?? 5)}
          onChange={(e) => patch({ guardian_disk_pct: Number(e.currentTarget.value) })}
        >
          {[3, 5, 10, 15].map((n) => (
            <option value={String(n)}>{n} %</option>
          ))}
        </select>
      </SettingRow>

      <SettingRow label="Reconnaissance faciale" desc="Prévue pour une prochaine version.">
        <span class="pill-status">Bientôt</span>
      </SettingRow>

      <div class="section-label">Historique des accès</div>
      <div class="muted settings-section-copy">
        Retrouve les accès à tes données dans Activité. Syn n'envoie aucune télémétrie.
      </div>
    </div>
  );
}

export function TabAide(): JSX.Element {
  const [llm] = createResource(() => ipc.llmStatus());
  return (
    <div>
      <div class="settings-h1">Aide</div>
      <div class="card">
        <div class="card-title">Syn 0.1.0</div>
        <div class="muted settings-card-copy no-action">
          Assistant local pour retrouver tes informations et agir sur ton appareil. Tes données
          restent sur ce Mac. Les actions sensibles demandent toujours ta confirmation.
        </div>
      </div>
      <div class="card">
        <div class="card-title">Diagnostic</div>
        <div class="diagnostic-line simple">
          <span>Moteur local</span>
          <span class="pill-status" classList={{ ok: llm()?.available }}>
            {llm()?.available ? "Disponible" : "Indisponible"}
          </span>
        </div>
        <div class="diagnostic-line">
          <span>Conversation</span>
          <span>{settings()?.chat_model}</span>
          <span class="pill-status" classList={{ ok: llm()?.chat_model_ready }}>
            {llm()?.chat_model_ready ? "Prêt" : "À télécharger"}
          </span>
        </div>
        <div class="diagnostic-line">
          <span>Recherche</span>
          <span>{settings()?.embed_model}</span>
          <span class="pill-status" classList={{ ok: llm()?.embed_model_ready }}>
            {llm()?.embed_model_ready ? "Prêt" : "À télécharger"}
          </span>
        </div>
      </div>
    </div>
  );
}
