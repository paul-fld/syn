import type { JSX } from "solid-js";

export function Toggle(props: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}): JSX.Element {
  return (
    <button
      class="toggle"
      classList={{ on: props.checked }}
      style={{ opacity: props.disabled ? 0.5 : 1 }}
      onClick={() => !props.disabled && props.onChange(!props.checked)}
      role="switch"
      aria-checked={props.checked}
    >
      <span class="knob" />
    </button>
  );
}

export function SettingRow(props: {
  label: string;
  desc?: string;
  children: JSX.Element;
}): JSX.Element {
  return (
    <div class="setting-row">
      <div class="info">
        <div class="label">{props.label}</div>
        {props.desc && <div class="desc">{props.desc}</div>}
      </div>
      {props.children}
    </div>
  );
}
