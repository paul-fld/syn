// Onglets Général, Compte, Personnalisation, Accessibilité.
import { Show, type JSX } from "solid-js";
import { Toggle, SettingRow } from "../components/Toggle";
import { ipc } from "../lib/ipc";
import { settings, refreshSettings, status } from "../lib/state";

const patch = async (p: Record<string, unknown>) => {
  await ipc.setSettings(p);
  refreshSettings();
};

export function TabGeneral(): JSX.Element {
  return (
    <div>
      <div class="settings-h1">Général</div>

      <SettingRow
        label="Niveau d'autonomie"
        desc="Règle le seuil au-dessus du plancher. Le plancher (action irréversible, vers une personne, financière) exige TOUJOURS une confirmation — quel que soit ce niveau."
      >
        <select
          class="select"
          value={settings()?.autonomy ?? "assiste"}
          onChange={(e) => patch({ autonomy: e.currentTarget.value })}
        >
          <option value="prudent">Prudent — tout est confirmé</option>
          <option value="assiste">Assisté — le réversible-local est automatique</option>
          <option value="autonome">Autonome — tout sauf le plancher</option>
        </select>
      </SettingRow>

      <SettingRow label="Brief de démarrage" desc="Au premier réveil du jour : agenda, tâches, engagements.">
        <Toggle
          checked={settings()?.startup_brief_enabled ?? true}
          onChange={(v) => patch({ startup_brief_enabled: v })}
        />
      </SettingRow>

      <SettingRow label="Heure-plancher du brief" desc="Jamais de brief avant cette heure.">
        <select
          class="select"
          value={String(settings()?.brief_floor_hour ?? 7)}
          onChange={(e) => patch({ brief_floor_hour: Number(e.currentTarget.value) })}
        >
          {[5, 6, 7, 8, 9, 10].map((h) => (
            <option value={String(h)}>{h}h00</option>
          ))}
        </select>
      </SettingRow>

      <SettingRow label="Débrief de fin de journée" desc="Bouclé aujourd'hui, glissé à demain, promesses en cours.">
        <Toggle
          checked={settings()?.daily_wrap_enabled ?? true}
          onChange={(v) => patch({ daily_wrap_enabled: v })}
        />
      </SettingRow>

      <SettingRow label="Lancement au démarrage" desc="Syn démarre avec la session et vit dans la barre des menus.">
        <Toggle checked={settings()?.autostart ?? false} onChange={(v) => patch({ autostart: v })} />
      </SettingRow>
    </div>
  );
}

export function TabCompte(): JSX.Element {
  return (
    <div>
      <div class="settings-h1">Compte</div>
      <div class="card">
        <div class="card-title">Identité locale</div>
        <div class="muted" style={{ "line-height": "1.6" }}>
          {status()?.email ?? "Aucune adresse renseignée"}
          <br />
          Syn ne demande <b>aucun compte cloud</b> : le mot de passe maître local est ton portail
          de sécurité. Le jour où Syn devient payant, l'activation se fera par une clé de licence
          utilisable hors-ligne — jamais un compte-pour-utiliser.
        </div>
      </div>
    </div>
  );
}

export function TabPersonnalisation(): JSX.Element {
  const setFormality = (f: string) =>
    patch({ voice: { ...settings()!.voice, formality: f } });
  const setAddress = (a: string) =>
    patch({ voice: { ...settings()!.voice, address_form: a.trim() || null } });
  return (
    <div>
      <div class="settings-h1">Personnalisation</div>

      <SettingRow
        label="Ton"
        desc="Tutoiement ou vouvoiement — dans la parole de Syn et les libellés prévus de l'interface. Une règle (ex. « #Vouvoie-moi ») prime sur ce réglage."
      >
        <select
          class="select"
          value={settings()?.voice.formality ?? "vous"}
          onChange={(e) => setFormality(e.currentTarget.value)}
        >
          <option value="vous">Vouvoiement</option>
          <option value="tu">Tutoiement</option>
        </select>
      </SettingRow>

      <SettingRow label="Forme d'adresse" desc="Comment Syn t'appelle (« Monsieur », un prénom… ou rien).">
        <input
          class="text-input"
          style={{ width: "180px" }}
          placeholder="—"
          value={settings()?.voice.address_form ?? ""}
          onChange={(e) => setAddress(e.currentTarget.value)}
        />
      </SettingRow>

      <SettingRow label="Thème" desc="V1 : thème sombre (celui des maquettes).">
        <select class="select" value="dark">
          <option value="dark">Sombre</option>
        </select>
      </SettingRow>
    </div>
  );
}

export function TabAccessibilite(): JSX.Element {
  return (
    <div>
      <div class="settings-h1">Accessibilité</div>

      <SettingRow label="Raccourci de la barre d'interaction" desc="Appelle Syn depuis n'importe où.">
        <span class="pill-status">⌥ Espace</span>
      </SettingRow>

      <SettingRow label="Entrée vocale (dictée)" desc="Transcription locale (whisper.cpp) — optionnelle, livrée tard dans la V1.">
        <Toggle
          checked={false}
          disabled
          onChange={() => {}}
        />
      </SettingRow>

      <SettingRow label="Sortie vocale" desc="Lecture des briefs à voix haute (locale).">
        <Toggle
          checked={false}
          disabled
          onChange={() => {}}
        />
      </SettingRow>

      <SettingRow label="Réduire les animations" desc="Limite les transitions de l'interface.">
        <Toggle checked={settings()?.reduce_motion ?? false} onChange={(v) => patch({ reduce_motion: v })} />
      </SettingRow>

      <SettingRow label="Texte plus grand" desc="Augmente la taille de police de l'interface.">
        <Toggle checked={settings()?.large_text ?? false} onChange={(v) => patch({ large_text: v })} />
      </SettingRow>
    </div>
  );
}
