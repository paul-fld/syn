import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { resolve } from "path";

export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "safari15",
    rollupOptions: {
      input: {
        main: resolve(import.meta.dirname, "index.html"),
        bar: resolve(import.meta.dirname, "bar.html"),
      },
    },
  },
});
