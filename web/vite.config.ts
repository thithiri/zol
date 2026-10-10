import { defineConfig } from "vite";

export default defineConfig({
  // src/wire.ts is a symlink to crates/zol-wire/ts/wire.ts.
  server: { fs: { allow: [".."] } },
  define: { "process.env": {} },
  test: { include: ["test/**/*.test.ts"] },
} as any);
