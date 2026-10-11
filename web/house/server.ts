// zol-house: HTTP service for single-player games against the house (Bun).
//
//   zol-house --config /home/zol/ops/house/house.json
//
// Routes (behind Caddy at /house/*; the page is /house.html):
//   GET  /house/info                        bankroll, stake, whether a game can start
//   POST /house/play   {session}            -> {game, tx}: create+join for the player to sign
//   POST /house/join   {game, tx}           the player's signed tx; the house co-signs and sends
//   GET  /house/game/:id                    timeline of one game
//   GET  /house/games/:session              the player's recent games
//   POST /house/withdraw {session}         -> {id, tx}: exit + settle for the player to sign
//   POST /house/withdraw/sign {id, tx}     the player's signed withdrawal; co-signed and sent
//   POST /house/fund   {session, memo?}     one deposit from the house's funding wallet
//   GET  /house/ztx/:txid                   confirmations of a Zcash transaction
// Build: bun build --compile --target=bun-linux-x64 house/server.ts --outfile dist-house/zol-house
import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { Keypair } from "@solana/web3.js";
import { House, HouseError, TERMINAL, type Game, type HouseConfig, type Store } from "./house";

interface FileConfig extends HouseConfig {
  listen: string; // host:port
  db: string;
  sessionSeedFile: string; // 64 hex chars
  feePayerFile: string; // Solana keypair JSON
}

class SqliteStore implements Store {
  constructor(private db: Database) {
    db.run("PRAGMA journal_mode=WAL");
    db.run("CREATE TABLE IF NOT EXISTS games (id TEXT PRIMARY KEY, player TEXT NOT NULL, state TEXT NOT NULL, json TEXT NOT NULL)");
    db.run("CREATE INDEX IF NOT EXISTS games_player ON games(player)");
    db.run("CREATE TABLE IF NOT EXISTS hits (key TEXT NOT NULL, t INTEGER NOT NULL)");
    db.run("CREATE INDEX IF NOT EXISTS hits_key ON hits(key, t)");
  }
  get(id: string) {
    const r = this.db.query("SELECT json FROM games WHERE id = ?").get(id) as { json: string } | null;
    return r ? (JSON.parse(r.json) as Game) : undefined;
  }
  put(g: Game) {
    this.db.query("INSERT OR REPLACE INTO games (id, player, state, json) VALUES (?, ?, ?, ?)").run(g.id, g.player, g.state, JSON.stringify(g));
  }
  active() {
    const q = `SELECT json FROM games WHERE state NOT IN (${TERMINAL.map(() => "?").join(",")})`;
    return (this.db.query(q).all(...TERMINAL) as { json: string }[]).map((r) => JSON.parse(r.json) as Game);
  }
  byPlayer(p: string) {
    return (this.db.query("SELECT json FROM games WHERE player = ?").all(p) as { json: string }[]).map((r) => JSON.parse(r.json) as Game);
  }
  count(key: string, since: number) {
    return (this.db.query("SELECT COUNT(*) AS n FROM hits WHERE key = ? AND t >= ?").get(key, since) as { n: number }).n;
  }
  bump(key: string) {
    this.db.query("INSERT INTO hits (key, t) VALUES (?, ?)").run(key, Date.now());
  }
}

const args = process.argv;
const cfgPath = args[args.indexOf("--config") + 1];
if (!cfgPath || args.indexOf("--config") < 0) {
  console.error("usage: zol-house --config <house.json>");
  process.exit(2);
}
const cfg = JSON.parse(readFileSync(cfgPath, "utf8")) as FileConfig;
const session = House.sessionFromSeedHex(readFileSync(cfg.sessionSeedFile, "utf8").trim());
const feePayer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(cfg.feePayerFile, "utf8"))));
const house = new House(cfg, session, feePayer, new SqliteStore(new Database(cfg.db)));

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body, (_, v) => (typeof v === "bigint" ? String(v) : v)), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });

async function route(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const p = url.pathname.replace(/\/+$/, "");
  // Caddy sets X-Forwarded-For; the service listens on localhost only.
  const ip = (req.headers.get("x-forwarded-for") ?? "local").split(",")[0].trim();
  const body = async () => {
    try { return (await req.json()) as any; } catch { throw new HouseError(400, "body must be JSON"); }
  };
  if (req.method === "GET" && p === "/house/info") return json(200, await house.info());
  if (req.method === "POST" && p === "/house/play") return json(200, await house.play(String((await body()).session ?? ""), ip));
  if (req.method === "POST" && p === "/house/join") {
    const b = await body();
    return json(200, await house.join(String(b.game ?? ""), String(b.tx ?? "")));
  }
  if (req.method === "POST" && p === "/house/withdraw") return json(200, await house.withdraw(String((await body()).session ?? "")));
  if (req.method === "POST" && p === "/house/withdraw/sign") {
    const b = await body();
    return json(200, await house.submitWithdraw(String(b.id ?? ""), String(b.tx ?? "")));
  }
  if (req.method === "POST" && p === "/house/fund") {
    const b = await body();
    return json(200, await house.fund(String(b.session ?? ""), ip, b.memo === undefined ? undefined : String(b.memo)));
  }
  let m: RegExpMatchArray | null;
  if (req.method === "GET" && (m = p.match(/^\/house\/game\/([0-9a-f]{64})$/))) return json(200, house.game(m[1]));
  if (req.method === "GET" && (m = p.match(/^\/house\/games\/([1-9A-HJ-NP-Za-km-z]{32,44})$/))) return json(200, house.gamesOf(m[1]));
  if (req.method === "GET" && (m = p.match(/^\/house\/ztx\/([0-9a-f]{64})$/))) return json(200, await house.zcashTx(m[1]));
  return json(404, { error: "not found" });
}

const [hostname, port] = cfg.listen.split(":");
Bun.serve({
  hostname,
  port: Number(port),
  async fetch(req) {
    try {
      return await route(req);
    } catch (e: any) {
      const status = e instanceof HouseError ? e.status : 500;
      if (status === 500) console.error(new Date().toISOString(), req.method, new URL(req.url).pathname, e?.stack ?? e);
      return json(status, { error: String(e?.message ?? e).split("\n")[0] });
    }
  },
});
console.log(`zol-house on ${cfg.listen}; house session ${session.keypair.publicKey.toBase58()}, fee payer ${feePayer.publicKey.toBase58()}`);

// Drive games every 3 s; one tick at a time.
let ticking = false;
setInterval(async () => {
  if (ticking) return;
  ticking = true;
  try {
    await house.tick();
  } catch (e: any) {
    console.error(new Date().toISOString(), "tick", e?.message ?? e);
  } finally {
    ticking = false;
  }
}, 3000);
