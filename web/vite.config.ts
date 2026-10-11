import { defineConfig } from "vite";

export default defineConfig({
  // src/wire.ts is a symlink to crates/zol-wire/ts/wire.ts.
  server: { fs: { allow: [".."] } },
  define: { "process.env": {} },
  // Pages: home (deposit, withdraw, the games), the coin flip and Up/Down (both from
  // coinflip.html, see scripts/pages.py), the auction, and the original two-player table.
  build: { rollupOptions: { input: { main: "index.html", coinflip: "coinflip.html", updown: "updown.html", auction: "auction.html", twoplayer: "two-player.html" } } },
  test: { include: ["test/**/*.test.ts"] },
} as any);
