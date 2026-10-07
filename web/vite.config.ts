/// <reference types="vitest/config" />
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// In development the browser talks to Vite only; the API routes are proxied to
// the API container (alias "api" in the dev group), so the app, the OIDC
// callback and the session cookie share one origin, as in production where the
// API serves the built app. Override the target with CANNERY_API_URL.
const api = process.env.CANNERY_API_URL ?? "http://api:8000";
const proxied = { target: api, changeOrigin: false };

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    // The dev server is reached through a reverse proxy at the host in
    // CANNERY_DEV_HOST (set by dev/web.sh).
    allowedHosts: process.env.CANNERY_DEV_HOST ? [process.env.CANNERY_DEV_HOST] : [],
    proxy: { "/api": proxied, "/auth": proxied, "/mcp": proxied },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: false,
    restoreMocks: true,
  },
});
