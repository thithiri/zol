// ZOL coin flip page. Plain DOM; see docs/phase5-coinflip.md.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;

import qrcode from "qrcode-generator";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { base58 } from "@scure/base";
import { Gateway, type DepositStatus } from "./gateway";
import { prepareDeposit } from "./deposit";
import { exportSession, hex, isBackedUp, loadOrCreateSession, markBackedUp, restoreSession, unhex, type Session } from "./session";
import { parsePayoutAddress } from "./ua";
import { allowedStakes, Coinflip, decodeBalance, decodeLedgerParams, decodePot, decodeReceipt, decodeTable, DLP, sendTx, decodeApp } from "./chain";

// ---------------------------------------------------------------- settings

interface Settings {
  gateway: string; // comma-separated: every operator's page API
  baseRpc: string;
  erRpc: string;
  ledger: string;
  app: string;
  vrfQueue: string;
  validator: string; // empty: ask the rollup (getIdentity)
  feePayer: string; // empty: the session key pays; else a dev relayer secret key (JSON array or base58)
}
const env = (import.meta as any).env ?? {};
const DEFAULTS: Settings = {
  gateway: env.VITE_GATEWAY ?? "http://127.0.0.1:8787",
  baseRpc: env.VITE_BASE_RPC ?? "http://127.0.0.1:28899",
  erRpc: env.VITE_ER_RPC ?? "http://127.0.0.1:27799",
  ledger: env.VITE_LEDGER ?? "3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y",
  app: env.VITE_APP ?? "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo",
  vrfQueue: env.VITE_VRF_QUEUE ?? "Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT",
  validator: env.VITE_VALIDATOR ?? "",
  feePayer: "",
};
const stored = JSON.parse(localStorage.getItem("zol:settings") ?? "{}");
// Saved settings hold every field; drop a base RPC that is only the old public default,
// so a new default (the operators' rate-limit-free proxy) takes effect.
if (stored.baseRpc === "https://api.devnet.solana.com") delete stored.baseRpc;
const settings: Settings = { ...DEFAULTS, ...stored };
const save = () => localStorage.setItem("zol:settings", JSON.stringify(settings));

// ---------------------------------------------------------------- state

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const session: Session = loadOrCreateSession(settings.app, localStorage);
const gateways = settings.gateway.split(",").map((u) => new Gateway(u.trim())).filter((g) => g.base);
const gw = gateways[0];
const base = new Connection(settings.baseRpc, "confirmed");
const er = new Connection(settings.erRpc, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey(settings.ledger), app: new PublicKey(settings.app) });

function feePayer(): Keypair {
  const s = settings.feePayer.trim();
  if (!s) return session.keypair;
  return Keypair.fromSecretKey(s.startsWith("[") ? Uint8Array.from(JSON.parse(s)) : base58.decode(s));
}
let potId: Uint8Array | null = location.hash.startsWith("#table=") ? unhex(location.hash.slice(7)) : null;

function log(msg: string, err = false) {
  const li = document.createElement("li");
  li.textContent = `${new Date().toLocaleTimeString()} ${msg}`;
  if (err) li.className = "err";
  $("log").prepend(li);
}
async function act(label: string, f: () => Promise<unknown>) {
  try {
    log(`${label}…`);
    const r = await f();
    log(`${label}: ok${typeof r === "string" ? ` ${r.slice(0, 16)}…` : ""}`);
  } catch (e: any) {
    log(`${label}: ${e.message ?? e}`, true);
  }
  void refresh();
}

// ---------------------------------------------------------------- deposit

$("session").textContent = session.keypair.publicKey.toBase58();

// ---------------------------------------------------------------- session backup
// A deposit link is offered only once the player has a copy of the session key.

function showBackupState() {
  const done = isBackedUp(settings.app, session, localStorage);
  $("backup-state").textContent = done ? "backed up" : "not backed up yet: deposits need a backup first";
  const btn = $<HTMLButtonElement>("deposit-btn");
  btn.disabled = !done;
  btn.textContent = done ? "Create deposit link" : "Back up your session key first (Session, above)";
  btn.title = done ? "" : "The session key is the only way to use or recover your balance";
}
$("backup-btn").onclick = () => {
  $<HTMLTextAreaElement>("backup-text").value = exportSession(settings.app, session);
  $("backup-out").hidden = false;
};
$("backup-copy").onclick = () => act("copy backup", () => navigator.clipboard.writeText(exportSession(settings.app, session)));
$("backup-download").onclick = () => {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([exportSession(settings.app, session) + "\n"], { type: "text/plain" }));
  a.download = `zol-session-${session.keypair.publicKey.toBase58().slice(0, 8)}.txt`;
  a.click();
  URL.revokeObjectURL(a.href);
};
$("backup-done").onclick = () => {
  markBackedUp(settings.app, session, localStorage);
  $("backup-out").hidden = true;
  showBackupState();
};
$("restore-btn").onclick = () => ($("restore-out").hidden = false);
$("restore-go").onclick = () => act("restore session", async () => {
  const text = $<HTMLTextAreaElement>("restore-text").value;
  const current = exportSession(settings.app, session);
  if (text.trim() === current) throw new Error("that is the session already in use");
  if (!isBackedUp(settings.app, session, localStorage) && !confirm("The current session key is not backed up. Replace it anyway? Any balance it holds would be lost.")) return;
  restoreSession(settings.app, text, localStorage);
  location.reload();
});
showBackupState();
$<HTMLInputElement>("payout").value = localStorage.getItem(`zol:payout:${settings.app}`) ?? "";

async function loadApp() {
  try {
    const a = await gw.app(settings.app);
    $("app-info").textContent = `${a.network} · ${a.state} · tip ${a.tip} · early credit ${a.early_depth} conf`;
    const sel = $<HTMLSelectElement>("amount");
    sel.innerHTML = a.denominations.map((d) => `<option value="${d}">${d / 1e8} ZEC</option>`).join("");
  } catch (e: any) {
    $("app-info").textContent = `gateway: ${e.message}`;
  }
}

$("deposit-btn").onclick = () => act("deposit link", async () => {
  const payout = $<HTMLInputElement>("payout").value.trim();
  const a = await gw.app(settings.app);
  parsePayoutAddress(payout, a.network); // validates before anything is signed
  localStorage.setItem(`zol:payout:${settings.app}`, payout);
  const d = await prepareDeposit(gateways, settings.app, session, payout, BigInt($<HTMLSelectElement>("amount").value), $<HTMLInputElement>("auto-exit").checked);
  const link = $<HTMLAnchorElement>("link");
  link.href = d.link;
  link.textContent = d.link;
  const qr = qrcode(0, "M");
  qr.addData(d.link);
  qr.make();
  $("qr").innerHTML = qr.createSvgTag({ cellSize: 3, margin: 2 });
  $("deposit-out").hidden = false;
  if (d.failed.length) log(`intent not registered at ${d.failed.map((f) => `${f.gateway} (${f.error})`).join(", ")}`, true);
  return `registered at ${d.registered.length}/${gateways.length} operators`;
});

async function pollDeposits() {
  try {
    const ds: DepositStatus[] = await gw.deposits(settings.app, hex(session.publicKey));
    $("deposits").innerHTML = ds.length
      ? ds.map((d) => `<li>${d.value / 1e8} ZEC · ${d.state} · ${d.confirmations} conf · ${d.cmx.slice(0, 12)}…</li>`).join("")
      : "<li>none yet</li>";
  } catch {
    /* gateway down: keep the last view */
  }
}

// ---------------------------------------------------------------- table

async function validator(): Promise<PublicKey> {
  if (settings.validator) return new PublicKey(settings.validator);
  const r = await fetch(settings.erRpc, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getIdentity" }) });
  return new PublicKey((await r.json()).result.identity);
}

/** The table and pot live in the rollup while delegated; read them where they are. */
async function readLive(key: PublicKey) {
  const b = await base.getAccountInfo(key);
  if (b && b.owner.equals(DLP)) return (await er.getAccountInfo(key)) ?? b;
  return b;
}

/** Solana fees come from the fee payer; say so plainly before a transaction would fail. */
async function requireFees() {
  const fp = feePayer().publicKey;
  const lamports = await base.getBalance(fp);
  if (lamports < 2_000_000) {
    throw new Error(`the fee payer ${fp.toBase58().slice(0, 8)}… has ${lamports / 1e9} SOL. Paste a funded devnet key into Settings → feePayer (empty means this session key pays)`);
  }
}
async function myBalance() {
  const b = await base.getAccountInfo(cf.pdas.balance(session.keypair.publicKey));
  return b ? decodeBalance(b.data) : null;
}

$("create-btn").onclick = () => act("create table", async () => {
  await requireFees();
  const stake = BigInt($<HTMLSelectElement>("stake").value);
  const id = crypto.getRandomValues(new Uint8Array(32));
  const ixs = [cf.createTable(feePayer().publicKey, id, stake, 1800n, await validator())];
  const bal = await myBalance();
  // The creator takes a seat in the same transaction when its balance covers the stake.
  const seat = bal && !bal.exiting && bal.amount >= stake;
  if (seat) ixs.push(cf.join(session.keypair.publicKey, id));
  const sig = await sendTx(base, ixs, feePayer(), seat ? [session.keypair] : []);
  potId = id;
  location.hash = `table=${hex(potId)}`;
  log(seat ? "you are seated; share the table link with the other player" : "table created; join once your deposit is credited");
  return sig;
});
$("open-btn").onclick = () => {
  potId = unhex($<HTMLInputElement>("table-id").value.trim());
  location.hash = `table=${hex(potId)}`;
  void refresh();
};
const need = () => {
  if (!potId) throw new Error("no table");
  return potId;
};
$("join-btn").onclick = () => act("join", async () => {
  await requireFees();
  return sendTx(base, [cf.join(session.keypair.publicKey, need())], feePayer(), [session.keypair]);
});
$("start-btn").onclick = () => act("start", async () => {
  await requireFees();
  return sendTx(base, [cf.start(feePayer().publicKey, need(), 600n)], feePayer(), [], 400_000);
});
// Rollup transactions are free: the session key pays even with no SOL.
$("flip-btn").onclick = () => act("flip", () => sendTx(er, [cf.flip(session.keypair.publicKey, need(), new PublicKey(settings.vrfQueue), crypto.getRandomValues(new Uint8Array(32)))], session.keypair));
$("finish-btn").onclick = () => act("finish", () => sendTx(er, [cf.finish(session.keypair.publicKey, need())], session.keypair));
$("cashout-btn").onclick = () => act("cash out", async () => {
  await requireFees();
  const pot = cf.pdas.pot(need());
  const r = decodeReceipt((await base.getAccountInfo(cf.pdas.receipt(pot)))!.data);
  return sendTx(base, [cf.cashOut(need(), r.players)], feePayer());
});
$("close-btn").onclick = () => act("close table", async () => {
  await requireFees();
  const t = decodeTable((await base.getAccountInfo(cf.pdas.table(need())))!.data);
  return sendTx(base, [cf.closeTable(need(), t.creator)], feePayer());
});
$("rescue-btn").onclick = () => act("rescue table", async () => {
  const t = decodeTable((await base.getAccountInfo(cf.pdas.table(need())))!.data);
  return sendTx(base, [cf.rescueTable(need(), t.validator)], feePayer());
});
// Exit and settle in one transaction: the balance is marked for exit and put into the app's
// next settlement, which the operators then pay on Zcash to the payout address.
$("exit-btn").onclick = () => act("exit and settle", async () => {
  await requireFees();
  const bal = await myBalance();
  if (!bal || bal.amount === 0n) throw new Error("nothing to exit");
  const app = decodeApp((await base.getAccountInfo(cf.pdas.app()))!.data);
  const me = session.keypair.publicKey;
  const ixs = bal.exiting ? [] : [cf.requestExit(me)];
  ixs.push(cf.settle(feePayer().publicKey, app.nextSettlement, [me]));
  const sig = await sendTx(base, ixs, feePayer(), bal.exiting ? [] : [session.keypair]);
  log(`settlement ${app.nextSettlement}: the operators pay it on Zcash in a few minutes`);
  return sig;
});
$("airdrop-btn").onclick = () => act("airdrop (local only)", async () => base.requestAirdrop(feePayer().publicKey, 1e9));

/** Stakes follow the ledger's entry fee: one deposit's net credited value per seat. */
async function loadStakes() {
  try {
    const c = await base.getAccountInfo(cf.pdas.config());
    const p = decodeLedgerParams(c!.data);
    $("stake").innerHTML = allowedStakes(p).map((s) => `<option value="${s}">${Number(s) / 1e8} ZEC (one deposit, after the ${p.entryFeeBps / 100}% entry fee)</option>`).join("");
  } catch (e: any) {
    log(`stakes: ${e.message}`, true);
  }
}

/** Enables only the steps that can succeed now, so a click is never ahead of the chain. */
function setButtons(s: { bal: bigint; exiting: boolean; table?: string; pot?: string; seated?: boolean; seats?: number; stake?: bigint; inRollup?: boolean }) {
  const on = (id: string, v: boolean) => ($<HTMLButtonElement>(id).disabled = !v);
  on("create-btn", true);
  on("join-btn", s.table === "Open" && s.pot === "Open" && !s.seated && (s.seats ?? 0) < 2 && !s.exiting && s.bal >= (s.stake ?? 0n));
  on("start-btn", s.table === "Open" && s.seats === 2);
  on("flip-btn", s.table === "Started" && !!s.inRollup && !!s.seated);
  on("finish-btn", s.table === "Landed" && !!s.inRollup);
  on("cashout-btn", s.table === "Finished" && s.pot === "Delegated" && !s.inRollup);
  on("close-btn", s.pot === "CashedOut" || s.pot === "Refunded");
  on("exit-btn", s.bal > 0n);
}

async function refresh() {
  let bal: Awaited<ReturnType<typeof myBalance>> = null;
  try {
    bal = await myBalance();
    $("balance").textContent = bal ? `${bal.amount} zat${bal.exiting ? " (exiting)" : ""}` : "no balance yet";
    $("sol").textContent = `${(await base.getBalance(feePayer().publicKey)) / 1e9} SOL (fee payer on Solana)`;
  } catch (e: any) {
    $("balance").textContent = `rpc: ${e.message}`;
  }
  const mine = { bal: bal?.amount ?? 0n, exiting: !!bal?.exiting };
  setButtons(mine);
  if (!potId) return;
  $<HTMLInputElement>("table-id").value = hex(potId);
  try {
    const tableKey = cf.pdas.table(potId);
    const pot = cf.pdas.pot(potId);
    const t = await readLive(tableKey);
    const p = await readLive(pot);
    const rAcc = await base.getAccountInfo(cf.pdas.receipt(pot));
    if (!t || !rAcc) {
      $("table").textContent = "table not found";
      return;
    }
    const table = decodeTable(t.data);
    const r = decodeReceipt(rAcc.data);
    const amounts = p ? decodePot(p.data).amounts : [];
    const me = session.keypair.publicKey;
    const inRollup = !!(await base.getAccountInfo(tableKey))?.owner.equals(DLP);
    const where = inRollup ? "rollup" : "Solana";
    setButtons({ ...mine, table: table.state, pot: r.state, seated: r.players.some((pl) => pl.equals(me)), seats: r.players.length, stake: table.stake, inRollup });
    $("table").innerHTML = `
      <div>Table <code>${tableKey.toBase58()}</code> · stake ${table.stake} zat · <b>${table.state}</b> (${where}) · pot ${r.state}</div>
      <div class="muted">Rollup validator (trusted for the outcome): <code>${table.validator.toBase58()}</code></div>
      <ol>${r.players.map((pl, i) => `<li>${pl.equals(me) ? "<b>you</b>" : pl.toBase58().slice(0, 8) + "…"} · buy-in ${r.buyIns[i]} · seat now ${amounts[i] ?? "?"}${table.winner === i ? " · <b>winner</b>" : ""}</li>`).join("")}</ol>`;
  } catch (e: any) {
    $("table").textContent = `table: ${e.message}`;
  }
}

// ---------------------------------------------------------------- settings form

const form = $("settings");
for (const k of Object.keys(DEFAULTS) as (keyof Settings)[]) {
  const row = document.createElement("label");
  row.innerHTML = `${k} <input name="${k}" value="${settings[k]}" ${k === "feePayer" ? 'type="password" placeholder="empty: session key pays"' : ""}/>`;
  form.appendChild(row);
}
$("settings-save").onclick = () => {
  for (const k of Object.keys(DEFAULTS) as (keyof Settings)[]) settings[k] = (form.querySelector(`[name=${k}]`) as HTMLInputElement).value;
  save();
  location.reload();
};

void loadApp();
void loadStakes();
void refresh();
void pollDeposits();
setInterval(pollDeposits, 5000);
// Solana reads: every 20 s (each action also refreshes at once), to stay inside RPC rate limits.
setInterval(refresh, 20_000);
