import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  esbuild: {
    target: "es2020",
  },
  build: {
    target: "es2020",
  },
  server: {
    host: "127.0.0.1",
    strictPort: false,
  },
});
