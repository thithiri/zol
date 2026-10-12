// The faucet the way the games page uses it: a fresh player registers a 0.01 TAZ deposit,
// the house sends it from op0's funding wallet (POST /house/fund), and this times the credit.
// The payout address is the funding wallet and auto-exit is on, so the TAZ goes back to it.
// Uses one of the house's daily grants and this address's one per day. Run from web/ with the
// tunnel up (see house/bankroll.ts):
//   npx vite-node house/faucet-smoke.ts
import { Connection, PublicKey } from "@solana/web3.js";
import { Buffer } from "buffer";
import { Coinflip, decodeBalance } from "../src/chain";
import { prepareDeposit } from "../src/deposit";
import { Gateway } from "../src/gateway";
import { hex, sessionFromSeed } from "../src/session";

const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const HOST = process.env.HOST ?? "https://zol.cash";
const ZCASHD = process.env.ZCASHD ?? "http://127.0.0.1:28300";
// FUND_URL=http://127.0.0.1:<port>/house/fund (a tunnel to the house) counts against the host's
// own address, not this one's, when this address has used its grant for the day.
const FUND = process.env.FUND_URL ?? `${HOST}/house/fund`;
const base = new Connection(`${HOST}/solana`, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"), app: new PublicKey(APP) });
const out = (o: object) => console.log(JSON.stringify({ t: new Date().toISOString().slice(11, 19), ...o }));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function http(url: string, body?: unknown) {
  const r = await fetch(url, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${url}: ${r.status} ${t}`);
  return JSON.parse(t);
}

const p = sessionFromSeed(crypto.getRandomValues(new Uint8Array(32)));
const me = p.keypair.publicKey;
const app = await new Gateway(`${HOST}/op0`).app(APP);
out({ step: "player", session: me.toBase58(), early_depth: app.early_depth, fund_me: (await http(`${HOST}/house/info`)).fund_me });

const funding = await http(`${ZCASHD}/admin/funding`);
const d = await prepareDeposit((app.operators ?? [`${HOST}/op0`]).map((u) => new Gateway(u)), APP, p, funding.ua, 1_000_000n, true);
const memo = Buffer.from(new URL(d.link.replace("zcash:", "zcash://x/")).searchParams.get("memo")!, "base64url").toString("utf8");
const t0 = Date.now();
const sent = await http(FUND, { session: hex(p.publicKey), memo });
out({ step: "house sent", txid: sent.txid });

const gw = new Gateway(`${HOST}/op0`);
let seen = 0;
for (;;) {
  const s = Math.round((Date.now() - t0) / 1000);
  const ds = await gw.deposits(APP, hex(p.publicKey)).catch(() => []);
  if (ds.length && !seen) {
    seen = s;
    out({ step: "deposit seen", seconds: s, confirmations: ds[0].confirmations });
  }
  const b = await base.getAccountInfo(cf.pdas.balance(me)).catch(() => null);
  if (b && decodeBalance(b.data).amount > 0n) {
    out({ step: "credited", seconds: s, balance: String(decodeBalance(b.data).amount) });
    break;
  }
  if (s > 900) throw new Error("not credited in 15 minutes");
  await sleep(3_000);
}
