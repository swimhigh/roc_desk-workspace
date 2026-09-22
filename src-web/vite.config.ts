import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  clearScreen: false,
  build: {
    outDir: "../standalone/dist",
    emptyOutDir: true,
  },
  server: {
    port: 1431,
    strictPort: true,
    host: host || false,
    watch: {
      ignored: ["**/src-tauri/**", "**/standalone/**", "**/lib/**"],
    },
  },
}));
