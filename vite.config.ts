import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  clearScreen: false,
  server: {
    // 5273, not cxmail's 5173 — both dev servers may run at once.
    port: 5273,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5274 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
