import { defineConfig } from "vitest/config";

// Pure-logic tests only (formatting, folder ordering, reply building). Components are checked
// with svelte-check and by running the app.
export default defineConfig({ test: { include: ["src/**/*.test.ts"], environment: "node" } });
