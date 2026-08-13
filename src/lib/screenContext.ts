import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, type ScreenContext } from "./ipc";

/** Masque Syn pour que la capture montre réellement ce qui se trouve derrière. */
export async function captureVisibleScreen(): Promise<ScreenContext> {
  const win = getCurrentWindow();
  let hidden = false;
  try {
    await win.hide();
    hidden = true;
    await new Promise((resolve) => setTimeout(resolve, 100));
    return await ipc.screenContext();
  } finally {
    if (hidden) {
      await win.show().catch(() => {});
      await win.setFocus().catch(() => {});
    }
  }
}
