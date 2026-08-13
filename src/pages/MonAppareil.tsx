// Mon appareil : capteurs système + diagnostic explicable (gardien).
import { createResource, For, Show, type JSX } from "solid-js";
import { Icon } from "../components/Icon";
import { ipc } from "../lib/ipc";

export function MonAppareil(): JSX.Element {
  const [data, { refetch }] = createResource(() => ipc.systemSnapshot());

  const snap = () => data()?.snapshot;

  return (
    <div class="page">
      <div class="page-title">Mon appareil</div>
      <div class="page-sub">
        Les capteurs de Syn sur cette machine. Lecture seule ; toute action système passerait par
        la porte de confirmation. Diagnostic utile sur les cas courants — pas un expert hardware.
      </div>

      <Show when={snap()} keyed>
        {(s: any) => (
          <>
            <div class="card">
              <div class="card-title">
                <Icon name="gauge" size={15} />
                Diagnostic
                <span class="spacer" />
                <button class="btn" onClick={refetch}>
                  Actualiser
                </button>
              </div>
              <div style={{ "line-height": "1.55", color: "var(--text-secondary)" }}>
                {data()?.explanation}
              </div>
            </div>

            <div style={{ display: "grid", "grid-template-columns": "1fr 1fr", gap: "12px" }}>
              <div class="card" style={{ "margin-bottom": "0" }}>
                <div class="card-title">
                  <Icon name="gauge" size={14} /> Processeur
                </div>
                <div style={{ "font-size": "22px", "font-weight": "600" }}>{Math.round(s.cpu_pct)} %</div>
                <div class="sub muted">{s.cpu_count} cœurs · {s.os}</div>
              </div>
              <div class="card" style={{ "margin-bottom": "0" }}>
                <div class="card-title">
                  <Icon name="brain" size={14} /> Mémoire
                </div>
                <div style={{ "font-size": "22px", "font-weight": "600" }}>
                  {s.mem_used_gb} / {s.mem_total_gb} Go
                </div>
                <div class="progress-track">
                  <div class="progress-fill" style={{ width: `${(s.mem_used_gb / s.mem_total_gb) * 100}%` }} />
                </div>
              </div>
            </div>

            <div class="section-label">Disques</div>
            <For each={s.disks}>
              {(d: any) => (
                <div class="row-line">
                  <Icon name="save" size={14} />
                  <span class="grow">{d.mount}</span>
                  <span class="sub">{d.free_gb} Go libres / {d.total_gb} Go</span>
                </div>
              )}
            </For>

            <Show when={s.battery}>
              <div class="section-label">Batterie</div>
              <div class="row-line">
                <Icon name="battery" size={14} />
                <span class="grow">{s.battery.pct} %</span>
                <span class="sub">{s.battery.charging ? "Sur secteur" : "Sur batterie"}</span>
              </div>
            </Show>

            <Show when={(s.temps ?? []).length > 0}>
              <div class="section-label">Températures</div>
              <For each={s.temps}>
                {(t: any) => (
                  <div class="row-line">
                    <Icon name="gauge" size={14} />
                    <span class="grow">{t.label}</span>
                    <span class="sub">{t.celsius} °C</span>
                  </div>
                )}
              </For>
            </Show>

            <div class="section-label">Processus les plus actifs</div>
            <For each={s.top_processes}>
              {(p: any) => (
                <div class="row-line">
                  <Icon name="play" size={13} />
                  <span class="grow">{p.name}</span>
                  <span class="sub">{p.cpu_pct} % CPU · {p.mem_mb} Mo</span>
                </div>
              )}
            </For>
          </>
        )}
      </Show>
    </div>
  );
}
