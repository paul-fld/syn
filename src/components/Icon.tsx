// Icônes du projet (dossier Icons/, jeu Lucide + icônes de marques).
// Les Lucide passent en currentColor ; les marques gardent leurs couleurs.
import { createMemo, type JSX } from "solid-js";

const files = import.meta.glob<string>("../assets/icons/*.svg", {
  query: "?raw",
  import: "default",
  eager: true,
});

const REGISTRY: Record<string, string> = {};
for (const [path, raw] of Object.entries(files)) {
  const name = path.split("/").pop()!.replace(".svg", "");
  REGISTRY[name] = raw;
}

const BRAND = new Set([
  "apple", "google", "microsoft", "slack", "github", "gmail", "outlook",
  "message", "calendrier", "apple-mail", "apple-calendar-ios",
]);

export function Icon(props: {
  name: string;
  size?: number;
  class?: string;
  style?: JSX.CSSProperties;
}): JSX.Element {
  const html = createMemo(() => {
    let raw = REGISTRY[props.name];
    if (!raw) return "";
    const size = props.size ?? 16;
    if (!BRAND.has(props.name)) {
      raw = raw
        .replace(/stroke="#000000"/g, 'stroke="currentColor"')
        .replace(/fill="#000000"/g, 'fill="currentColor"')
        .replace(/stroke="white"/g, 'stroke="currentColor"')
        .replace(/fill="white"/g, 'fill="currentColor"');
    }
    raw = raw
      .replace(/(height)="24"/, `$1="${size}"`)
      .replace(/(width)="24"/, `$1="${size}"`)
      .replace(/width="(\d+)" height="(\d+)"/, `width="${size}" height="${size}"`);
    return raw;
  });
  return (
    <span
      class={`icon ${props.class ?? ""}`}
      style={{ display: "inline-flex", "align-items": "center", ...props.style }}
      innerHTML={html()}
    />
  );
}
