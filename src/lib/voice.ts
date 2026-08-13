// Catalogue de chaînes templatées (doc Règles §7) : les libellés du chrome
// honorent UNIQUEMENT formality + address_form, via des variantes prévues.
// La parole générée de Syn honore tout le profil (côté backend).

import type { VoiceProfile } from "./ipc";

type Entry = { tu: string; vous: string };

const CATALOG: Record<string, Entry> = {
  "greeting": { tu: "Bonjour{address},", vous: "Bonjour{address}," },
  "ask.placeholder": { tu: "Demander à Syn", vous: "Demander à Syn" },
  "rules.title": { tu: "Tes règles", vous: "Vos règles" },
  "rules.prompt": {
    tu: "Écris une nouvelle règle que tu veux que Syn intègre",
    vous: "Écrivez une nouvelle règle que vous voulez que Syn intègre",
  },
  "rules.placeholder": {
    tu: "Exemple : #Tu peux t'arrêter à partir de 19h00",
    vous: "Exemple : #Tu peux t'arrêter à partir de 19h00",
  },
  "archives.title": { tu: "Ton activité", vous: "Votre activité" },
  "knowledge.sub": {
    tu: "Gère ce que Syn a appris de ta vie numérique. Tout est local, chiffré, supprimable.",
    vous: "Gérez ce que Syn a appris de votre vie numérique. Tout est local, chiffré, supprimable.",
  },
  "brief.empty": {
    tu: "Rien de particulier aujourd'hui. Syn te fera signe s'il voit quelque chose d'utile — jamais sans raison.",
    vous: "Rien de particulier aujourd'hui. Syn vous fera signe s'il voit quelque chose d'utile — jamais sans raison.",
  },
  "lock.hint": { tu: "Entre ton mot de passe maître", vous: "Entrez votre mot de passe maître" },
};

export function label(key: string, voice: VoiceProfile | null | undefined): string {
  const entry = CATALOG[key];
  if (!entry) return key;
  const form = voice?.formality === "tu" ? entry.tu : entry.vous;
  const address = voice?.address_form ? ` ${voice.address_form}` : "";
  return form.replace("{address}", address);
}
