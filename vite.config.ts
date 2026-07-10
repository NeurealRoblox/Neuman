import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST ?? "127.0.0.1";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host,
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: process.env.TAURI_ENV_DEBUG ? false : "esbuild",
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
    outDir: "dist",
    assetsDir: "",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: "desktop-[hash].js",
        chunkFileNames: "chunk-[hash].js",
        assetFileNames: "desktop-[hash][extname]",
      },
    },
  },
});
