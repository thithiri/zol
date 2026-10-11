import { defineConfig } from "vite";

export default defineConfig({
  // src/wire.ts is a symlink to crates/zol-wire/ts/wire.ts.
  server: { fs: { allow: [".."] } },
  define: { "process.env": {} },
  // Pages: the two-player table, the single-player games against the house, the auction.
  build: { rollupOptions: { input: { main: "index.html", house: "house.html", auction: "auction.html" } } },
  test: { include: ["test/**/*.test.ts"] },
} as any);
