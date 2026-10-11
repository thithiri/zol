// The single-player page: deposit, flip against the house, get paid. The house service
// (web/house) builds and pays for the Solana transactions; this page signs with the
// player's session key and flips the coin in the rollup (free).
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import qrcode from "qrcode-generator";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Gateway, type AppInfo, type DepositStatus } from "./gateway";
import { prepareDeposit } from "./deposit";
import { exportSession, hex, isBackedUp, loadOrCreateSession, markBackedUp } from "./session";
import { parsePayoutAddress } from "./ua";
import { Coinflip, decodeBalance, sendTx } from "./chain";

const env = (import.meta as any).env ?? {};
const S = {
  gateway: env.VITE_GATEWAY ?? "http://127.0.0.1:8787",
  baseRpc: env.VITE_BASE_RPC ?? "http://127.0.0.1:28899",
  erRpc: env.VITE_ER_RPC ?? "http://127.0.0.1:27799",
  ledger: env.VITE_LEDGER ?? "3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y",
  app: env.VITE_APP ?? "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo",
  vrfQueue: env.VITE_VRF_QUEUE ?? "Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT",
  house: env.VITE_HOUSE ?? "/house",
};
const AMOUNT = 1_000_000n; // 0.01 TAZ, the only deposit this page takes
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const gw = new Gateway(S.gateway);
const base = new Connection(S.baseRpc, "confirmed");
const er = new Connection(S.erRpc, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey(S.ledger), app: new PublicKey(S.app) });
const session = loadOrCreateSession(S.app, localStorage);
const me = session.keypair.publicKey;
const K = (k: string) => `zol:house:${k}:${me.toBase58()}`;
const solTx = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;
const erTx = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=custom&customUrl=${encodeURIComponent(S.erRpc)}`;
const zec = (zat: number | bigint) => `${(Number(zat) / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, "")} TAZ`;
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

function showError(e: unknown) {
  $("err").textContent = String((e as any)?.message ?? e).split("\n")[0];
  setTimeout(() => ($("err").textContent = ""), 12_000);
}
async function house<T = any>(path: string, body?: unknown): Promise<T> {
  const r = await fetch(`${S.house}${path}`, body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error ?? `house: ${r.status}`);
  return j as T;
}

// ---------------------------------------------------------------- session backup

function backupState() {
  const done = isBackedUp(S.app, session, localStorage);
  $("backup-state").innerHTML = done ? '<span class="ok">Backed up.</span>' : '<span class="err">Not backed up yet: needed before you deposit.</span>';
  $<HTMLButtonElement>("deposit-btn").disabled = !done;
  $<HTMLButtonElement>("deposit-btn").title = done ? "" : "Back up your session first (above)";
}
$("backup-btn").onclick = () => {
  $("backup-text").textContent = exportSession(S.app, session);
  $("backup-out").hidden = false;
};
$("backup-copy").onclick = () => navigator.clipboard.writeText(exportSession(S.app, session)).catch(showError);
$("backup-done").onclick = () => {
  markBackedUp(S.app, session, localStorage);
  $("backup-out").hidden = true;
  backupState();
};

// ---------------------------------------------------------------- 1. deposit

let app: AppInfo | null = null;
let link: string | null = localStorage.getItem(K("link"));
let sessionUa: string | null = localStorage.getItem(K("ua"));
$<HTMLInputElement>("payout").value = localStorage.getItem("zol:house:payout") ?? "";

function showLink() {
  if (!link || !sessionUa) return;
  $("deposit-out").hidden = false;
  const a = $<HTMLAnchorElement>("link");
  a.href = link;
  $("session-ua").textContent = sessionUa;
  const qr = qrcode(0, "M");
  qr.addData(link);
  qr.make();
  $("qr").innerHTML = qr.createSvgTag({ cellSize: 3, margin: 2 });
}

$("deposit-btn").onclick = async () => {
  try {
    app ??= await gw.app(S.app);
    const payout = $<HTMLInputElement>("payout").value.trim();
    parsePayoutAddress(payout, app.network);
    localStorage.setItem("zol:house:payout", payout);
    // Auto-exit: once the game ends, the balance is settled and paid without another step.
    const d = await prepareDeposit([gw], S.app, session, payout, AMOUNT, true);
    link = d.link;
    sessionUa = d.session.ua;
    // Deposits from before this link (e.g. from the two-player page, which shares the
    // session) are not part of this game: list only those from here on.
    localStorage.setItem(K("since"), String(d.app.tip));
    localStorage.setItem(K("link"), link);
    localStorage.setItem(K("ua"), sessionUa);
    showLink();
  } catch (e) {
    showError(e);
  }
};

$("fund-btn").onclick = async () => {
  const b = $<HTMLButtonElement>("fund-btn");
  b.disabled = true;
  try {
    // The browser Buffer polyfill has no "base64url": map it to standard base64 first.
    const b64url = link ? new URL(link.replace("zcash:", "zcash://x/")).searchParams.get("memo") ?? "" : "";
    const memo = b64url ? Buffer.from(b64url.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8") : undefined;
    const r = await house<{ txid: string }>("/fund", { session: hex(session.publicKey), ...(memo?.startsWith("ZOL1:") ? { memo } : {}) });
    localStorage.setItem(K("fundtx"), r.txid);
    void pollDeposits();
  } catch (e) {
    showError(e);
    b.disabled = false;
  }
};

let deposits: DepositStatus[] = [];
async function pollDeposits() {
  try {
    app ??= await gw.app(S.app);
    deposits = await gw.deposits(S.app, hex(session.publicKey));
    const items: string[] = [];
    const fundTx = localStorage.getItem(K("fundtx"));
    if (fundTx && !deposits.some((d) => d.txid === fundTx)) {
      const z = await house(`/ztx/${fundTx}`).catch(() => null);
      items.push(`<li class="wait">the house sent 0.01 TAZ to your session address <span class="badge">zcash</span> <code>${fundTx.slice(0, 16)}…</code> ${z?.height ? `in block ${z.height}` : "waiting for a block"}</li>`);
    }
    const since = Number(localStorage.getItem(K("since")) ?? 0);
    for (const d of deposits.filter((x) => x.height >= since).slice(-3)) {
      const need = app.early_depth;
      const conf = Math.min(d.confirmations, need);
      items.push(`<li>payment seen: ${zec(d.value)} in block ${d.height} <span class="badge">zcash</span> ${d.txid ? `<code>${d.txid.slice(0, 16)}…</code>` : ""}</li>`);
      if (d.state === "credited") items.push(`<li>credited to your balance on the Solana ledger</li>`);
      else if (d.state === "seen") items.push(`<li class="wait">waiting for confirmations: ${conf} of ${need} (about ${Math.max(1, need - conf) * 25} s)</li>`);
      else items.push(`<li class="err">${d.state}: ${d.state === "held" ? "an operator will look at it" : "it will be refunded to your address"}</li>`);
    }
    $("deposit-timeline").innerHTML = items.join("");
  } catch {
    /* keep the last view */
  }
}

// ---------------------------------------------------------------- 2. play

let balance = 0n;
let stake = 0n;
let houseCanPlay = false;
let gameId: string | null = localStorage.getItem(K("game"));
let game: any = null;
let flipping = false;

async function pollBalance() {
  try {
    const b = await base.getAccountInfo(cf.pdas.balance(me));
    const bal = b ? decodeBalance(b.data) : null;
    balance = bal?.amount ?? 0n;
    $("balance").textContent = bal ? `${zec(bal.amount)}${bal.exiting ? " (being paid out)" : ""}` : "none yet";
  } catch {
    /* rate limited or offline: keep the last value */
  }
}

async function pollHouse() {
  try {
    const i = await house("/info");
    stake = BigInt(i.stake);
    houseCanPlay = i.can_play;
    $("house-info").innerHTML = `Stake ${zec(stake)} each; the winner takes ${zec(2n * stake)}. House bankroll ${zec(BigInt(i.bankroll))}${i.can_play ? "" : ` · <span class="err">${BigInt(i.bankroll) < stake ? "the house is out of bankroll" : "the house is busy"}</span>`}.`;
  } catch (e) {
    $("house-info").innerHTML = `<span class="err">house unavailable: ${esc(String((e as any)?.message ?? e))}</span>`;
  }
}

const LIVE = ["awaiting_signature", "seated", "started", "in_rollup", "landed", "finishing", "returned", "cashed_out"];

function renderButtons() {
  const live = game && LIVE.includes(game.state);
  const canPlay = balance >= stake && stake > 0n && houseCanPlay && !live;
  $<HTMLButtonElement>("play-btn").disabled = !canPlay;
  $("s-play").classList.toggle("off", !(canPlay || live || game));
  $("s-payout").classList.toggle("off", !game || !["settled", "paid", "final"].includes(game.state));
}

$("play-btn").onclick = async () => {
  const b = $<HTMLButtonElement>("play-btn");
  b.disabled = true;
  $("result").textContent = "";
  try {
    const p = await house<{ game: string; tx: string }>("/play", { session: me.toBase58() });
    const tx = Transaction.from(Buffer.from(p.tx, "base64"));
    tx.partialSign(session.keypair); // your seat at the table: the only thing you sign on Solana
    const signed = Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
    gameId = p.game;
    localStorage.setItem(K("game"), gameId);
    game = await house("/join", { game: p.game, tx: signed });
    render();
  } catch (e) {
    showError(e);
    renderButtons();
  }
};

async function flipIfReady() {
  if (!game || game.state !== "in_rollup" || flipping || localStorage.getItem(K(`flipped:${game.id}`))) return;
  flipping = true;
  try {
    const potId = Uint8Array.from(Buffer.from(game.id, "hex"));
    const sig = await sendTx(er, [cf.flip(me, potId, new PublicKey(S.vrfQueue), crypto.getRandomValues(new Uint8Array(32)))], session.keypair);
    localStorage.setItem(K(`flipped:${game.id}`), JSON.stringify({ sig, t: new Date().toISOString().slice(11, 19) }));
  } catch (e) {
    showError(e); // the house flips for you after a minute anyway
  } finally {
    flipping = false;
  }
}

function eventLi(e: any) {
  const link = e.sig && e.chain === "solana" ? ` <a href="${solTx(e.sig)}" target="_blank" rel="noopener">tx ↗</a>`
    : e.sig && e.chain === "rollup" ? ` <a href="${erTx(e.sig)}" target="_blank" rel="noopener">rollup tx ↗</a>`
    : e.sig && e.chain === "zcash" ? ` <code>${e.sig.slice(0, 16)}…</code> <span class="zconf" data-tx="${e.sig}"></span>` : "";
  return `<li>${esc(e.step)}${e.chain ? `<span class="badge">${e.chain}</span>` : ""}${link} <span class="muted" style="font-size:12px">${e.t}</span></li>`;
}

const PAYOUT_STEPS = /^(settlement|paid on Zcash|final)/;
function render() {
  if (!game) return renderButtons();
  // Stored as {sig, t}; earlier versions stored the bare signature.
  const flipRaw = localStorage.getItem(K(`flipped:${game.id}`));
  const flip: { sig: string; t: string } | null = !flipRaw ? null : flipRaw.startsWith("{") ? JSON.parse(flipRaw) : { sig: flipRaw, t: "" };
  // Retries are the house's business; the player sees the steps.
  const evs = game.events.filter((e: any) => !e.step.startsWith("retrying"));
  if (flip) evs.splice(evs.findIndex((e: any) => e.step.startsWith("table in the")) + 1, 0, { step: "you flipped the coin; the VRF picks heads or tails", t: flip.t, chain: "rollup", sig: flip.sig });
  $("game-timeline").innerHTML = evs.filter((e: any) => !PAYOUT_STEPS.test(e.step)).map(eventLi).join("")
    + (game.state === "in_rollup" ? '<li class="wait">flipping…</li>' : LIVE.includes(game.state) ? '<li class="wait">working…</li>' : "");
  $("payout-timeline").innerHTML = evs.filter((e: any) => PAYOUT_STEPS.test(e.step)).map(eventLi).join("")
    + (game.state === "settled" ? '<li class="wait">the operators are building and signing the Zcash payment…</li>' : game.state === "paid" ? '<li class="wait">getting final (about 2 h)…</li>' : "");
  if (game.playerWon === true) $("result").innerHTML = `<span class="ok">You won ${zec(2n * BigInt(game.stake))}!</span>`;
  else if (game.playerWon === false) $("result").textContent = "The house won this one.";
  if (["expired", "failed"].includes(game.state)) $("result").innerHTML = `<span class="err">${esc(game.error ?? game.state)}</span>`;
  renderButtons();
}

async function pollGame() {
  if (!gameId) return renderButtons();
  try {
    game = await house(`/game/${gameId}`);
    render();
    await flipIfReady();
  } catch {
    /* keep the last view */
  }
}

async function pollZcashConfs() {
  for (const el of Array.from(document.querySelectorAll<HTMLElement>(".zconf"))) {
    const z = await house(`/ztx/${el.dataset.tx}`).catch(() => null);
    if (z?.found) el.textContent = z.height ? `· ${z.confirmations} confirmations` : "· in the mempool";
  }
}

// ---------------------------------------------------------------- start

backupState();
showLink();
void Promise.all([pollBalance(), pollHouse(), pollDeposits(), pollGame()]).then(renderButtons);
setInterval(pollDeposits, 6_000);
setInterval(pollGame, 3_000);
setInterval(pollBalance, 15_000);
setInterval(pollHouse, 30_000);
setInterval(pollZcashConfs, 20_000);
