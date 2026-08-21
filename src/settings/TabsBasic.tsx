// Onglets Général, Compte, Personnalisation, Accessibilité.
import type { JSX } from "solid-js";
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
        desc="Définit les actions que Syn peut effectuer sans confirmation."
      >
        <select
          class="select"
          value={settings()?.autonomy ?? "assiste"}
          onChange={(e) => patch({ autonomy: e.currentTarget.value })}
        >
          <option value="prudent">Prudent : toujours confirmer</option>
          <option value="assiste">Assisté : actions locales simples</option>
          <option value="autonome">Autonome : sauf actions sensibles</option>
        </select>
      </SettingRow>

      <SettingRow
        label="Brief de démarrage"
        desc="Affiche l'agenda et les tâches au premier lancement du jour."
      >
        <Toggle
          checked={settings()?.startup_brief_enabled ?? true}
          onChange={(v) => patch({ startup_brief_enabled: v })}
        />
      </SettingRow>

      <SettingRow label="Heure du brief" desc="N'affiche aucun brief avant cette heure.">
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

      <SettingRow label="Bilan de fin de journée" desc="Résume les tâches terminées, reportées et en cours.">
        <Toggle
          checked={settings()?.daily_wrap_enabled ?? true}
          onChange={(v) => patch({ daily_wrap_enabled: v })}
        />
      </SettingRow>

      <SettingRow label="Ouvrir avec la session" desc="Lance Syn à l'ouverture de ta session.">
        <Toggle checked={settings()?.autostart ?? false} onChange={(v) => patch({ autostart: v })} />
      </SettingRow>
    </div>
  );
}

export function TabNotifications(): JSX.Element {
  const enabled = () => settings()?.notifications_enabled ?? true;
  return (
    <div>
      <div class="settings-h1">Notifications</div>

      <SettingRow label="Notifications de Syn" desc="Affiche les informations qui demandent ton attention.">
        <Toggle
          checked={enabled()}
          onChange={(value) => patch({ notifications_enabled: value })}
        />
      </SettingRow>
      <SettingRow label="Sourdine" desc="Suspend toutes les notifications jusqu'à sa désactivation.">
        <Toggle
          checked={settings()?.notifications_muted ?? false}
          disabled={!enabled()}
          onChange={(value) => patch({ notifications_muted: value })}
        />
      </SettingRow>
      <SettingRow label="Son" desc="Joue un son pour les alertes importantes et urgentes.">
        <Toggle
          checked={settings()?.notification_sound ?? true}
          disabled={!enabled()}
          onChange={(value) => patch({ notification_sound: value })}
        />
      </SettingRow>
      <SettingRow label="Priorité minimale" desc="Masque les notifications moins importantes.">
        <select
          class="select"
          disabled={!enabled()}
          value={settings()?.notification_min_priority ?? "info"}
          onChange={(event) => patch({ notification_min_priority: event.currentTarget.value })}
        >
          <option value="info">Toutes</option>
          <option value="important">Importantes et urgentes</option>
          <option value="urgent">Urgentes uniquement</option>
        </select>
      </SettingRow>

      <div class="section-label">Types de notifications</div>
      <SettingRow label="Résumé du jour" desc="Agenda, tâches et rappels du jour.">
        <Toggle checked={settings()?.notify_briefs ?? true} disabled={!enabled()} onChange={(value) => patch({ notify_briefs: value })} />
      </SettingRow>
      <SettingRow label="Agenda" desc="Événements qui commencent bientôt.">
        <Toggle checked={settings()?.notify_events ?? true} disabled={!enabled()} onChange={(value) => patch({ notify_events: value })} />
      </SettingRow>
      <SettingRow label="Échéances" desc="Engagements arrivant à leur terme.">
        <Toggle checked={settings()?.notify_commitments ?? true} disabled={!enabled()} onChange={(value) => patch({ notify_commitments: value })} />
      </SettingRow>
      <SettingRow label="État de l'appareil" desc="Stockage, température et batterie.">
        <Toggle checked={settings()?.notify_system ?? true} disabled={!enabled()} onChange={(value) => patch({ notify_system: value })} />
      </SettingRow>
      <SettingRow label="Règles" desc="Alertes créées par tes règles personnelles.">
        <Toggle checked={settings()?.notify_rules ?? true} disabled={!enabled()} onChange={(value) => patch({ notify_rules: value })} />
      </SettingRow>

      <div class="section-label">Mode travail</div>
      <SettingRow label="Pendant le mode travail" desc="Choisis les notifications autorisées pendant une session de concentration.">
        <select
          class="select"
          disabled={!enabled()}
          value={settings()?.work_notification_policy ?? "urgent"}
          onChange={(event) => patch({ work_notification_policy: event.currentTarget.value })}
        >
          <option value="urgent">Urgentes uniquement</option>
          <option value="relevant">Urgentes, agenda et échéances</option>
        </select>
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
        <div class="settings-card-value">{status()?.email ?? "Aucune adresse renseignée"}</div>
        <div class="muted">
          Cette adresse identifie ton espace sur cet appareil. Aucun compte en ligne n'est requis.
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
        desc="Choisis comment Syn s'adresse à toi."
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

      <SettingRow
        label="Nom utilisé"
        desc="Indique le prénom ou le titre que Syn doit employer."
      >
        <input
          class="text-input"
          style={{ width: "180px" }}
          placeholder="Aucun"
          value={settings()?.voice.address_form ?? ""}
          onChange={(e) => setAddress(e.currentTarget.value)}
        />
      </SettingRow>

      <SettingRow
        label="Langue des réponses"
        desc="Syn répond dans ta langue. En automatique, il la reconnaît à tes phrases. Sa langue de travail interne reste l'anglais, celle que comprennent le mieux les modèles et les services."
      >
        <select
          class="select"
          value={settings()?.answer_language ?? "auto"}
          onChange={(e) => patch({ answer_language: e.currentTarget.value })}
        >
          <option value="auto">Automatique</option>
          <option value="fr">Français</option>
          <option value="en">English</option>
        </select>
      </SettingRow>

      <SettingRow label="Apparence" desc="Choisis le thème de l'interface.">
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

      <SettingRow
        label="Raccourci de la barre d'interaction"
        desc="Ouvre la barre depuis n'importe quelle application."
      >
        <span class="pill-status">⌥ Espace</span>
      </SettingRow>

      <SettingRow
        label="Dictée"
        desc="Dicte tes demandes à Syn. La reconnaissance se fait sur l'appareil : ta voix ne quitte pas le Mac."
      >
        <Toggle
          checked={settings()?.voice_input_enabled ?? false}
          onChange={(v) => patch({ voice_input_enabled: v })}
        />
      </SettingRow>

      <SettingRow label="Lecture à voix haute" desc="Lit les réponses de Syn avec la voix du système.">
        <Toggle
          checked={settings()?.voice_output_enabled ?? false}
          onChange={(v) => patch({ voice_output_enabled: v })}
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
