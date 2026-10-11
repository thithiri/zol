import { defineConfig } from "vite";

export default defineConfig({
  // src/wire.ts is a symlink to crates/zol-wire/ts/wire.ts.
  server: { fs: { allow: [".."] } },
  define: { "process.env": {} },
  // Two pages: the two-player table and the single-player game against the house.
  build: { rollupOptions: { input: { main: "index.html", house: "house.html" } } },
  test: { include: ["test/**/*.test.ts"] },
} as any);
