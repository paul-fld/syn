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
          <Icon name="database" size={15} /> Ce que Syn stocke
        </div>
        <div class="muted" style={{ "line-height": "1.7" }}>
          {stats()?.items ?? "…"} éléments indexés · {stats()?.embeddings ?? "…"} fragments ·
          base chiffrée de {stats() ? fmtBytes(stats()!.db_bytes) : "…"}
          <br />
          Emplacement : <span class="mono">{stats()?.data_dir}</span>
        </div>
      </div>

      <div class="card">
        <div class="card-title">
          <Icon name="download" size={15} /> Export complet
        </div>
        <div class="muted" style={{ "margin-bottom": "10px", "line-height": "1.6" }}>
          Tes données t'appartiennent : la base (chiffrée) et le fichier de méta se copient tels
          quels. Avec ton mot de passe maître ou ta phrase de récupération, elles se rouvrent sur
          n'importe quelle machine avec Syn.
        </div>
        <button
          class="btn"
          onClick={async () => {
            await ipc.openSource(await ipc.dataDirPath()).catch(() => {});
          }}
        >
          Ouvrir le dossier de données
        </button>
      </div>

      <div class="card">
        <div class="card-title" style={{ color: "var(--danger)" }}>
          <Icon name="ban" size={15} /> Purge complète
        </div>
        <div class="muted" style={{ "margin-bottom": "10px", "line-height": "1.6" }}>
          Supprime définitivement la mémoire, l’index et les clés locales de Syn sur cet appareil.
        </div>
        <Show
          when={purgeArmed()}
          fallback={
            <button class="btn danger" onClick={() => setPurgeArmed(true)}>
              Purger toutes mes données…
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
      <SettingRow label="Index & mémoire" desc="Base SQLite chiffrée (SQLCipher AES-256). Reconstructible depuis les sources.">
        <span class="pill-status">{stats() ? fmtBytes(stats()!.db_bytes) : "…"}</span>
      </SettingRow>
      <SettingRow label="Accès aux fichiers" desc="Autorisation macOS et indexation automatique gérées dans Connecteurs.">
        <span class="pill-status">{index()?.folders.length ?? 0} dossier(s)</span>
      </SettingRow>
      <div class="section-label">Modèles locaux ({llm()?.runtime ?? "…"})</div>
      <Show when={!llm()?.available}>
        <div class="rule-feedback">
          Moteur d'inférence indisponible : {llm()?.detail}. Le retrieval fonctionne ; la
          génération est signalée indisponible (mode dégradé).
        </div>
      </Show>
      <For each={llm()?.installed_models ?? []}>
        {(m) => (
          <div class="row-line">
            <Icon name="package" size={14} />
            <span class="grow">{m}</span>
            <Show when={m.startsWith(settings()?.chat_model.split(":")[0] ?? "")}>
              <span class="pill-status ok">modèle actif</span>
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
          <Icon name="baby" size={15} /> Bientôt
        </div>
        <div class="muted" style={{ "line-height": "1.7" }}>
          Superviser l'appareil d'un enfant est presque un produit à part entière : cadre légal
          spécifique (mineurs) et privilèges systèmes profonds. Ce module est repoussé à une
          version ultérieure plutôt que d'être livré à moitié sûr.
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
          <input class="text-input" style={{ flex: "1", "min-width": "160px" }} type="password" placeholder="Actuel" value={cur()} onInput={(e) => setCur(e.currentTarget.value)} />
          <input class="text-input" style={{ flex: "1", "min-width": "160px" }} type="password" placeholder="Nouveau (8+ caractères)" value={neu()} onInput={(e) => setNeu(e.currentTarget.value)} />
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
        <div class="muted" style={{ "margin-bottom": "10px", "line-height": "1.6" }}>
          Sans mot de passe ni phrase, les données sont <b>irrécupérables</b> — c'est le prix du
          vrai chiffrement local. Régénérer invalide l'ancienne phrase.
        </div>
        <Show
          when={phrase()}
          fallback={
            <div style={{ display: "flex", gap: "8px" }}>
              <input class="text-input" type="password" placeholder="Mot de passe maître" value={rpPw()} onInput={(e) => setRpPw(e.currentTarget.value)} />
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
        desc="Garde la clé dans le trousseau macOS : déverrouillage par ta session / biométrie, sans retaper le mot de passe."
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
        label="Escalade cloud"
        desc="OFF par défaut. Si activée : uniquement la requête problématique part vers un modèle cloud, jamais la mémoire ; chaque usage est signalé. (Aucun fournisseur configuré dans ce build : l'egress reste fermé.)"
      >
        <Toggle
          checked={false}
          disabled
          onChange={() => {}}
        />
      </SettingRow>

      <SettingRow
        label="Contenu des fichiers autorisés"
        desc="Inclus dans l’autorisation unique aux fichiers. Le contenu reste local et chiffré ; les fichiers système et techniques sont exclus."
      >
        <span class="pill-status ok">Inclus</span>
      </SettingRow>

      <SettingRow label="Budget de rareté" desc="Plafond de surfaçages proactifs par jour. L'urgent passe toujours.">
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

      <SettingRow label="Gardien — seuil disque" desc="Alerte quand l'espace libre passe sous ce seuil.">
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

      <SettingRow label="Reconnaissance faciale" desc="Biométrie de tiers : repoussée à une version ultérieure (V2). Absente de ce build.">
        <span class="pill-status">V2</span>
      </SettingRow>

      <div class="section-label">Accès aux données</div>
      <div class="muted" style={{ "line-height": "1.6" }}>
        Les accès significatifs aux connecteurs sont tracés — consultables dans <b>Activité →
        Accès aux données</b>. Zéro télémétrie : rien ne quitte cette machine.
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
        <div class="muted" style={{ "line-height": "1.8" }}>
          Assistant de vie numérique <b>local-first</b> : mémoire + récupération + action, posées
          sur ta vie numérique, que tu possèdes entièrement.
          <br />
          · Local par défaut — l'inférence, la mémoire et l'index restent sur cette machine.
          <br />
          · Plancher humain — aucune action irréversible, externe ou financière sans ta
          confirmation.
          <br />
          · Proactivité rare et explicable — jamais « Syn a deviné ».
        </div>
      </div>
      <div class="card">
        <div class="card-title">Diagnostic</div>
        <div class="muted" style={{ "line-height": "1.8" }}>
          Runtime : {llm()?.runtime} — {llm()?.available ? "disponible" : `indisponible (${llm()?.detail ?? "?"})`}
          <br />
          Modèle de conversation : {settings()?.chat_model} {llm()?.chat_model_ready ? "✓" : "✗ (à télécharger)"}
          <br />
          Modèle d'embedding : {settings()?.embed_model} {llm()?.embed_model_ready ? "✓" : "✗ (à télécharger)"}
        </div>
      </div>
    </div>
  );
}
