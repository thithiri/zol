// The single-player game: add funds with shielded ZEC, flip as often as the balance allows,
// cash out to the player's Zcash wallet. The house service (web/house) builds and pays for
// the Solana transactions; this page signs with the player's session key (held by a passkey
// where the browser supports it) and flips the coin in the rollup.
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import qrcode from "qrcode-generator";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Gateway, type AppInfo, type DepositStatus } from "./gateway";
import { prepareDeposit } from "./deposit";
import { exportSession, hex, isBackedUp, markBackedUp, readSession, restoreSession, sessionKind, storeSession, type Session } from "./session";
import { parsePayoutAddress } from "./ua";
import { Coinflip, decodeBalance, decodePot, decodePrice, decodeTable, sendTx, UPDOWN, ZEC_USD_FEED, type TableAcc } from "./chain";
import { createPasskeySeed, NO_PRF_HELP, passkeysAvailable, prfSupported, seedFromPasskey } from "./passkey";

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
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const gw = new Gateway(S.gateway);
const base = new Connection(S.baseRpc, "confirmed");
const er = new Connection(S.erRpc, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey(S.ledger), app: new PublicKey(S.app) });
const solTx = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;
const erTx = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=custom&customUrl=${encodeURIComponent(S.erRpc)}`;
const taz = (zat: number | bigint) => `${(Number(zat) / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, "")} TAZ`;
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
const signB64 = (b64: string, s: Session) => {
  const tx = Transaction.from(Buffer.from(b64, "base64"));
  tx.partialSign(s.keypair); // the only thing the player signs on Solana
  return Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64");
};

// ---------------------------------------------------------------- coin faces
// Heads: the ZOL Z (two rows and the violet crossing). Tails: a five-petal flower for Zakura.
// Drawn here, not official artwork.

const cell = (x: number, y: number) => `<rect x="${x}" y="${y}" width="5" height="5"/>`;
const zMark = (fill: string, cross: string) =>
  `<g fill="${fill}">${[0, 6, 12, 18].map((x) => cell(x, 0) + cell(x, 18)).join("")}</g><g fill="${cross}">${cell(12, 6)}${cell(6, 12)}</g>`;
const zSvg = (fill: string, cross: string) =>
  `<svg viewBox="0 0 100 100" aria-label="Z heads"><g transform="translate(13.2 13.2) scale(3.2)">${zMark(fill, cross)}</g></svg>`;
const flowerSvg = (petal: string, heart: string) =>
  `<svg viewBox="0 0 100 100" aria-label="Zakura flower tails"><g transform="translate(50 50)">
    ${[0, 72, 144, 216, 288].map((a) => `<path transform="rotate(${a})" d="M0 -6 C-14 -14 -16 -32 -6 -40 L0 -34 L6 -40 C16 -32 14 -14 0 -6Z" fill="${petal}"/>`).join("")}
    <circle r="8" fill="${heart}"/></g></svg>`;
const FACE = { H: zSvg("#edecfa", "#a374ff"), T: flowerSvg("#edecfa", "#45f0c3") };
const ICON = { H: `<svg viewBox="0 0 23 23" aria-label="Z heads">${zMark("currentColor", "currentColor")}</svg>`, T: flowerSvg("currentColor", "currentColor") };
/** A whole coin around a face: an edge milled in square cells, a rim, a struck face. */
function coinSvg(inner: string) {
  const notches = Array.from({ length: 60 }, (_, i) => `<rect x="-2.2" y="-77.5" width="4.4" height="4.4" transform="rotate(${i * 6})"/>`).join("");
  return `<svg viewBox="-80 -80 160 160" aria-hidden="true">
    <defs>
      <radialGradient id="cf" cx="35%" cy="28%" r="80%"><stop offset="0" stop-color="#33276b"/><stop offset=".6" stop-color="#1e1545"/><stop offset="1" stop-color="#140c2e"/></radialGradient>
    </defs>
    <circle r="80" fill="#0c0720"/><g fill="#a374ff">${notches}</g>
    <circle r="69" fill="#1a1139" stroke="#3a3068" stroke-width="1.5"/>
    <circle r="62" fill="url(#cf)"/>
    <circle r="58" fill="none" stroke="#edecfa" stroke-opacity=".12" stroke-width="1"/>
    <g transform="translate(-44 -44) scale(.88)">${inner}</g>
  </svg>`;
}
function face(f: "H" | "T") {
  $("coin").innerHTML = coinSvg(FACE[f].replace("<svg ", '<svg width="100" height="100" '));
}
$("ico-heads").innerHTML = ICON.H.replace("<svg ", '<svg class="ico" ');
$("ico-tails").innerHTML = ICON.T.replace("<svg ", '<svg class="ico" ');
face("H");

// Close buttons on the deposit and withdraw panels.
for (const b of Array.from(document.querySelectorAll<HTMLButtonElement>("button.close"))) b.onclick = () => ($(b.dataset.close!).hidden = true);

// ---------------------------------------------------------------- the player key

let session: Session | null = readSession(S.app, localStorage);

async function begin(f: () => Promise<Session>) {
  try {
    session = await f();
    boot();
  } catch (e) {
    const msg = String((e as any)?.message ?? e);
    if (msg === NO_PRF_HELP) {
      // Show the advice where the buttons are, and point at the session-key option.
      $("pk-help").textContent = msg;
      $("pk-help").hidden = false;
      $("nopk-new").classList.add("primary");
    } else {
      showError(e);
    }
  }
}
$("pk-new").onclick = () => begin(async () => storeSession(S.app, await createPasskeySeed(S.app), localStorage, "passkey"));
$("pk-login").onclick = () => begin(async () => storeSession(S.app, await seedFromPasskey(S.app), localStorage, "passkey"));
$("nopk-new").onclick = () => begin(async () => storeSession(S.app, crypto.getRandomValues(new Uint8Array(32)), localStorage, "backup"));
$("restore-go").onclick = () => begin(async () => restoreSession(S.app, $<HTMLInputElement>("restore-text").value, localStorage));
// A shared demo player, published on purpose so visitors can play at once. Anyone can play its
// balance; its withdrawals can only go to the payout address fixed at its first deposit.
const DEMO_SESSION = "zolsess1:7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo:d7e17f86d3b628b63d521b02231073fdc1efa736c7dd12d7e55e386cb66a9e6c:4400e123";
$("demo-new").onclick = () => begin(async () => restoreSession(S.app, DEMO_SESSION, localStorage));
$("logout-btn").onclick = () => {
  if (!confirm("Switch player? You can come back with your passkey, or with this player's session key.")) return;
  localStorage.removeItem(`zol:session:${S.app}`);
  location.reload();
};
// Show and hide the session key; having seen it counts as saving it (deposits need that).
$("backup-btn").onclick = () => {
  const show = $("backup-out").hidden;
  $("backup-text").textContent = show ? exportSession(S.app, session!) : "";
  $("backup-out").hidden = !show;
  $("backup-btn").textContent = show ? "hide session key" : "show session key";
  if (show) markBackedUp(S.app, session!, localStorage);
  renderKey();
};
function renderKey() {
  const kind = sessionKind(S.app, localStorage);
  const ok = isBackedUp(S.app, session!, localStorage);
  $("key-kind").innerHTML = kind === "passkey" ? "held by your passkey" : ok ? "session key, saved" : '<span class="err">session key, NOT saved yet</span>';
  $("backup-btn").hidden = kind === "passkey";
  // An unsaved session key: a red dot on the key button until it is shown once.
  $("key-warn").hidden = kind === "passkey" || ok;
}
// The key button (top right of the balance card) opens the player-key panel.
$("key-btn").onclick = () => {
  const open = $("key-panel").hidden;
  $("key-panel").hidden = !open;
  $("key-btn").setAttribute("aria-expanded", String(open));
};

// ---------------------------------------------------------------- state

const K = (k: string) => `zol:house:${k}:${session!.keypair.publicKey.toBase58()}`;
let app: AppInfo | null = null;
let stake = 0n;
let minExit = 0n;
/** The player's ledger balance on Solana, and whether it is being paid out. */
let balance = 0n;
let exiting = false;
/** The house's record of the player's current table (a session) or withdrawal. */
let game: any = null;
/** The session's seats in the rollup [house, player], while it is there. */
let seats: [bigint, bigint] | null = null;
/** The table's state in the rollup. */
let tableState = "";
/** After a session closes, its seat until the ledger balance shows it (reads can lag). */
let pending: { seat: bigint; before: bigint; t: number } | null = null;
let busy = false;
/** Which page this is (the `data-page` of index.html, coinflip.html, updown.html), and its game. */
type Mode = "coinflip" | "updown";
const PAGE = (document.body.dataset.page ?? "coinflip") as "home" | Mode;
const mode: Mode = PAGE === "updown" ? "updown" : "coinflip";
/** ZEC/USD samples for the chart (the last 90 s). */
const samples: { t: number; p: bigint }[] = [];
/** The table as the rollup last showed it, and the latest ZEC/USD price (8 decimals). */
let lastTable: TableAcc | null = null;
let price: bigint | null = null;
const tableGame = (): Mode => (liveSession()?.game === "updown" ? "updown" : "coinflip");
const usd = (p: bigint) => `$${(Number(p) / 1e8).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;

const SESSION_LIVE = ["awaiting_signature", "seated", "started", "in_rollup", "landed", "finishing", "returned", "cashed_out"];
const liveSession = () => game && game.kind !== "withdraw" && SESSION_LIVE.includes(game.state) ? game : null;
const inRollup = () => liveSession()?.state === "in_rollup" && seats !== null;
/** What the player has: the ledger balance plus their seat at a live table. */
const total = () => balance + (liveSession() && seats ? seats[1] : 0n) + (pending ? pending.seat : 0n);

async function pollBalance() {
  try {
    const b = await base.getAccountInfo(cf.pdas.balance(session!.keypair.publicKey));
    const bal = b ? decodeBalance(b.data) : null;
    balance = bal?.amount ?? 0n;
    exiting = !!bal?.exiting;
    // The closed table's seat shows up in the ledger balance: stop adding it.
    if (pending && (balance !== pending.before || Date.now() - pending.t > 60_000)) pending = null;
  } catch {
    /* rate limited: keep the last value */
  }
  render();
}
let houseNote = "";
/** The house's faucet ("fund me"): one deposit of `fundZat` per player, `fundLeft` left today. */
let fundZat = 0n;
let fundLeft = 0;
async function pollHouse() {
  try {
    const i = await house("/info");
    stake = BigInt(i.stake);
    minExit = BigInt(i.min_exit ?? 0);
    fundZat = BigInt(i.fund_me?.zat ?? 0);
    fundLeft = Number(i.fund_me?.left_today ?? 0);
    // A stake is a tenth of a 0.01 TAZ deposit after the entry fee: 0.000997, shown as 0.001.
    houseNote = i.can_play || inRollup() ? "" : BigInt(i.bankroll) < stake ? "The house is out of bankroll for now" : "The house is busy; try in a minute";
  } catch {
    // Usually a rate-limited RPC on the operators' side: keep the last note, retry soon.
    if (!stake) houseNote = "Connecting to the house…";
    setTimeout(() => void pollHouse(), 5_000);
  }
  render();
}

function render() {
  $("balance").textContent = taz(total());
  const s = liveSession();
  const n = stake > 0n ? total() / stake : 0n;
  const unit = mode === "updown" ? "bet" : "flip";
  $("flips").textContent = stake === 0n ? " " : n > 0n ? `${n} ${unit}${n === 1n ? "" : "s"}' worth` : "deposit to play";
  const f = s?.flips ?? 0;
  const won = s?.wins ?? 0, tied = s?.ties ?? 0;
  $("table-note").textContent = s && f > 0
    ? `This table: ${won} won · ${f - won - tied} lost${tied ? ` · ${tied} tied` : ""}${lastResults ? `  ${lastResults}` : ""}`
    : "";
  const perStake = `${(Number(stake) / 1e8).toFixed(3)} TAZ`;
  $("stake-note").textContent = houseNote || (stake === 0n ? " " : mode === "updown" ? `${perStake} per bet · a right call pays 1.9×` : `${perStake} per flip`);
  // The game's buttons once its table is in the rollup; before that, one button that starts it.
  $("coin").hidden = mode !== "coinflip";
  $("updown").hidden = mode !== "updown";
  const ready = inRollup() && tableGame() === mode;
  for (const id of ["heads", "tails"]) $(id).hidden = !(ready && mode === "coinflip");
  for (const id of ["up-btn", "down-btn"]) $(id).hidden = !(ready && mode === "updown");
  const start = $<HTMLButtonElement>("start-table");
  start.hidden = ready;
  const closing = s && ["finishing", "returned", "cashed_out"].includes(s.state);
  const other = s && !closing && tableGame() !== mode;
  start.textContent = closing ? "Closing table…" : other ? `Close your ${tableGame() === "updown" ? "Up/Down" : "coin-flip"} table` : s || busy ? "Starting table…" : "Start table";
  start.disabled = busy || (!!s && !other) || stake === 0n || (!other && balance < stake) || exiting;
  const canFlip = ready && !s.stop && tableState !== "Flipping" && seats![0] >= stake && seats![1] >= stake;
  for (const id of ["heads", "tails"]) $<HTMLButtonElement>(id).disabled = busy || stake === 0n || !canFlip;
  const win = (stake * (UPDOWN.payoutBps - 10_000n)) / 10_000n;
  const canBet = ready && !s.stop && !lastTable?.bet.open && price !== null && seats![1] >= stake && seats![0] >= win;
  for (const id of ["up-btn", "down-btn"]) $<HTMLButtonElement>(id).disabled = busy || stake === 0n || !canBet;
  $<HTMLButtonElement>("cashout-btn").disabled = busy || total() === 0n || !!localStorage.getItem(K("withdraw-live"));
  renderFaucet();
}

/** The faucet card (games page): offered while the player has nothing and the house can fund;
 * kept, with its progress, from the house's send until the deposit is credited. */
function renderFaucet() {
  const sent = !!localStorage.getItem(K("fundtx"));
  $("faucet").hidden = PAGE !== "home" || total() > 0n || fundZat === 0n || (!sent && fundLeft <= 0);
  $("faucet-addr").hidden = sent || !!localStorage.getItem(payoutKey());
  $("faucet-btn").hidden = sent;
}

// ---------------------------------------------------------------- add funds

const payoutKey = () => K("payout");
function renderAddress() {
  const a = localStorage.getItem(payoutKey());
  $("addr-step").hidden = !!a;
  $("addr-known").hidden = !a;
  if (a) $("addr-short").textContent = `${a.slice(0, 14)}…${a.slice(-6)}`;
}
$("add-btn").onclick = async () => {
  $("add").hidden = !$("add").hidden;
  $("withdraw").hidden = true;
  renderAddress();
  if ($("add").hidden) return;
  try {
    app ??= await gw.app(S.app);
    $("amounts").innerHTML = app.denominations
      .map((d) => {
        const flips = stake > 0n ? BigInt(d) / stake : 0n;
        return `<button class="primary" data-amount="${d}">${taz(d)}${flips > 0n ? ` · ${flips} flip${flips > 1n ? "s" : ""}` : ""}</button>`;
      })
      .join("");
    for (const b of Array.from($("amounts").querySelectorAll<HTMLButtonElement>("button"))) b.onclick = () => deposit(BigInt(b.dataset.amount!));
  } catch (e) {
    showError(e);
  }
};

/** Registers a deposit of `amount` with every operator; `show` puts its QR code on screen. */
async function deposit(amount: bigint, show = true): Promise<boolean> {
  try {
    if (sessionKind(S.app, localStorage) === "backup" && !isBackedUp(S.app, session!, localStorage)) {
      $("key-panel").hidden = false;
      $("backup-btn").click();
      throw new Error("Your session key is shown at the top: save it, then deposit again");
    }
    app ??= await gw.app(S.app);
    let addr = localStorage.getItem(payoutKey());
    if (!addr) {
      addr = $<HTMLInputElement>("payout").value.trim();
      const raw = parsePayoutAddress(addr, app.network);
      // On a new device the player must give the same address as before: a deposit naming
      // another one would be voided.
      const c = await gw.payoutCheck(S.app, hex(session!.publicKey), hex(raw));
      if (c.bound && !c.matches) throw new Error("This player already cashes out to a different address. Enter the one you used before.");
      localStorage.setItem(payoutKey(), addr);
    }
    // Register with every operator (the app lists them), not only the one this page talks to.
    const gws = app.operators?.length ? app.operators.map((u) => new Gateway(u)) : [gw];
    const d = await prepareDeposit(gws, S.app, session!, addr, amount, false);
    localStorage.setItem(K("since"), String(d.app.tip));
    localStorage.setItem(K("link"), d.link);
    localStorage.setItem(K("ua"), d.session.ua);
    if (show) showPay(d.link, d.session.ua, amount);
    renderAddress();
    return true;
  } catch (e) {
    showError(e);
    return false;
  }
}
function showPay(link: string, ua: string, amount: bigint) {
  $("pay").hidden = false;
  $<HTMLAnchorElement>("link").href = link;
  $("pay-amount").textContent = taz(amount);
  $("session-ua").textContent = ua;
  const qr = qrcode(0, "M");
  qr.addData(link);
  qr.make();
  $("qr").innerHTML = qr.createSvgTag({ cellSize: 3, margin: 2 });
  // The house's "fund me" stays available in the API but is not offered on the page.
}
/** Asks the house to send its one deposit to this player's registered deposit link. */
async function fundMe(msg: HTMLElement, sentText: string, otherwise: string): Promise<boolean> {
  try {
    const link = localStorage.getItem(K("link")) ?? "";
    // The browser Buffer polyfill has no "base64url": map it to standard base64 first.
    const m = new URL(link.replace("zcash:", "zcash://x/")).searchParams.get("memo") ?? "";
    const memo = Buffer.from(m.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
    const r = await house<{ txid: string }>("/fund", { session: hex(session!.publicKey), ...(memo.startsWith("ZOL1:") ? { memo } : {}) });
    localStorage.setItem(K("fundtx"), r.txid);
    msg.hidden = false;
    msg.innerHTML = `<span class="ok">${sentText}</span>`;
    void pollDeposits();
    return true;
  } catch (e) {
    // Say why right here (most often: the once-a-day limit for this network address).
    msg.hidden = false;
    msg.innerHTML = `<span class="err">${esc(String((e as any)?.message ?? e).split("\n")[0])}.</span> ${otherwise}`;
    return false;
  }
}
const VALAR = '<a href="https://faucet.testnet.valargroup.dev" target="_blank" rel="noopener">Valar\'s faucet</a>';
$("fund-btn").onclick = async () => {
  if (await fundMe($("fund-msg"), "Sent: 0.01 TAZ is on its way to your address.", `Get testnet ZEC from ${VALAR} and pay the QR code instead.`)) $("fund-btn").hidden = true;
};
$("faucet-btn").onclick = async () => {
  const b = $<HTMLButtonElement>("faucet-btn");
  b.disabled = true;
  try {
    // The deposit form reads the payout address from its own field.
    if (!localStorage.getItem(payoutKey())) $<HTMLInputElement>("payout").value = $<HTMLInputElement>("faucet-payout").value;
    if (!(await deposit(fundZat, false))) return;
    await fundMe($("faucet-msg"), "Sent. It's credited after one Zcash block, usually within a minute or two.", `Or get testnet ZEC from ${VALAR} and use Deposit Zcash.`);
  } finally {
    b.disabled = false;
    renderFaucet();
  }
};

async function pollDeposits() {
  try {
    app ??= await gw.app(S.app);
    const since = Number(localStorage.getItem(K("since")) ?? Number.MAX_SAFE_INTEGER);
    const ds: DepositStatus[] = (await gw.deposits(S.app, hex(session!.publicKey))).filter((d) => d.height >= since);
    const items: string[] = [];
    const fundTx = localStorage.getItem(K("fundtx"));
    if (fundTx && !ds.some((d) => d.txid === fundTx)) items.push('<li class="wait">the house sent 0.01 TAZ; waiting for a Zcash block…</li>');
    for (const d of ds.slice(-3)) {
      if (d.state === "credited") items.push(`<li>${taz(d.value)} received and added to your balance</li>`);
      else if (d.state === "seen") items.push(`<li class="wait">${taz(d.value)} received; confirming ${Math.min(d.confirmations, app.early_depth)} of ${app.early_depth} blocks…</li>`);
      else items.push(`<li class="err">${taz(d.value)} ${d.state}: ${d.state === "held" ? "an operator will look at it" : "it will be refunded to your address"}</li>`);
    }
    $("deposit-tl").innerHTML = items.join("");
    $("faucet-tl").innerHTML = items.join("");
    if (ds.some((d) => d.state === "credited")) void pollBalance();
  } catch {
    /* keep the last view */
  }
}

// ---------------------------------------------------------------- flip

const coin = $("coin");
let call: "H" | "T" = "H";
$("heads").onclick = () => play("H");
$("tails").onclick = () => play("T");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const potIdOf = (id: string) => Uint8Array.from(Buffer.from(id, "hex"));

/** The table and its seats as the rollup has them now. */
async function readRollup(id: string) {
  const [ti, pi] = await er.getMultipleAccountsInfo([cf.pdas.table(potIdOf(id)), cf.pdas.pot(potIdOf(id))]);
  if (!ti || !pi) return null;
  const t = decodeTable(ti.data);
  const a = decodePot(pi.data).amounts;
  lastTable = t;
  return { t, seats: [a[0], a[1]] as [bigint, bigint] };
}

/** The table's last results for the player (seat 1), oldest first: ■ won, □ lost. */
let lastResults = "";
function resultDots(t: { flips: number; results: bigint }) {
  const n = Math.min(t.flips, 12);
  return Array.from({ length: n }, (_, i) => ((t.results >> BigInt(n - 1 - i)) & 1n ? "■" : "□")).join("");
}

/** Rings the side the player called; the other fades while the coin is in the air. */
function markCall(c: "H" | "T") {
  $("heads").classList.toggle("chosen", c === "H");
  $("tails").classList.toggle("chosen", c === "T");
  $("heads").classList.toggle("unchosen", c !== "H");
  $("tails").classList.toggle("unchosen", c !== "T");
}
const sideName = (c: "H" | "T") => (c === "H" ? "Z heads" : "Zakura flower tails");

/** Opens a table for this session: one signature, then it moves into the rollup (once). */
async function setupTable(): Promise<string> {
  $("status").textContent = "Setting up your table…";
  const p = await house<{ game: string; tx?: string; resume?: boolean }>("/play", { session: session!.keypair.publicKey.toBase58(), game: mode });
  const id = p.game;
  localStorage.setItem(K("game"), id);
  if (!p.resume) game = await house("/join", { game: id, tx: signB64(p.tx!, session!) });
  $("status").textContent = "Moving your table into the rollup…";
  for (let i = 0; i < 90 && game?.state !== "in_rollup"; i++) {
    await sleep(1_000);
    game = await house(`/game/${id}`).catch(() => game);
    if (["failed", "expired", "done", "final"].includes(game?.state)) throw new Error(game.error ?? `the table is ${game.state}`);
  }
  if (game?.state !== "in_rollup") throw new Error("the table is taking long to reach the rollup; try again in a minute");
  const r = await readRollup(id).catch(() => null);
  if (r) {
    seats = r.seats;
    tableState = r.t.state;
  }
  return id;
}

$("start-table").onclick = async () => {
  if (liveSession() && tableGame() !== mode) {
    await house("/stop", { session: session!.keypair.publicKey.toBase58() }).catch(showError);
    $("status").textContent = "Closing your table…";
    return;
  }
  busy = true;
  render();
  try {
    await setupTable();
    $("status").textContent = mode === "updown" ? "Table ready: will ZEC go up or down in the next 30 seconds?" : "Table ready: call it.";
  } catch (e) {
    $("status").textContent = " ";
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

async function play(c: "H" | "T") {
  call = c;
  markCall(c);
  busy = true;
  render();
  face(c);
  coin.classList.add("spin");
  try {
    const id = inRollup() ? (liveSession()!.id as string) : await setupTable();
    await flipNow(id);
  } catch (e) {
    coin.classList.remove("spin");
    for (const b of ["heads", "tails"]) $(b).classList.remove("chosen", "unchosen");
    $("status").textContent = " ";
    showError(e);
  } finally {
    busy = false;
    render();
  }
}

/** One flip in the rollup: ask the VRF, then watch the table until the coin lands. */
async function flipNow(id: string) {
  $("status").textContent = `Flipping… you called ${sideName(call)}`;
  let r = await readRollup(id);
  for (let i = 0; i < 10 && !r; i++) {
    await sleep(500);
    r = await readRollup(id);
  }
  if (!r) throw new Error("your table is not in the rollup yet; try again");
  const before = r.t.flips;
  await sendTx(er, [cf.flip(session!.keypair.publicKey, potIdOf(id), new PublicKey(S.vrfQueue), crypto.getRandomValues(new Uint8Array(32)))], session!.keypair);
  tableState = "Flipping";
  const t0 = Date.now();
  for (;;) {
    await sleep(350);
    r = await readRollup(id).catch(() => null);
    if (r && r.t.flips > before) break;
    if (Date.now() - t0 > 70_000) throw new Error("the coin is taking long to land; the house will close the table and nothing is lost");
  }
  seats = r.seats;
  tableState = r.t.state;
  lastResults = resultDots(r.t);
  const won = r.t.winner === 1;
  coin.classList.remove("spin");
  for (const b of ["heads", "tails"]) $(b).classList.remove("unchosen");
  const other = call === "H" ? "T" : "H";
  face(won ? call : other);
  $("status").innerHTML = won
    ? `<span class="ok">${call === "H" ? "Heads" : "Tails"}! You win +${taz(stake)}</span>`
    : `${other === "H" ? "Heads" : "Tails"}. The house wins this one.`;
  if (game) {
    game.flips = r.t.flips;
    game.wins = (game.wins ?? 0) + (won ? 1 : 0);
  }
}

// ---------------------------------------------------------------- ZEC Up/Down

/** ZEC/USD as the rollup has it (the program bets and settles on this account). */
async function pollPrice() {
  try {
    const a = await er.getAccountInfo(ZEC_USD_FEED);
    if (!a) return;
    price = decodePrice(a.data).price;
    $("price").textContent = usd(price);
    samples.push({ t: Date.now(), p: price });
    while (samples.length && samples[0].t < Date.now() - 90_000) samples.shift();
  } catch {
    /* keep the last price */
  }
  drawChart();
  if (!busy) render();
}

/**
 * The last minute of ZEC/USD; while a bet is open, its entry price as a dashed line, the
 * line green when the call is winning and red when losing, the lead in dollars, and a bar
 * counting down the round (pulsing in its last 5 s).
 */
function drawChart() {
  const W = 300, H = 96, span = 60_000, now = Date.now();
  const pts = samples.filter((x) => x.t >= now - span);
  const bet = lastTable?.bet.open ? lastTable.bet : null;
  const spark = $("spark");
  if (pts.length < 2) return;
  const vals = pts.map((x) => Number(x.p)).concat(bet ? [Number(bet.openPrice)] : []);
  let lo = Math.min(...vals), hi = Math.max(...vals);
  const pad = Math.max((hi - lo) * 0.15, 1e6); // at least $0.01 of headroom
  lo -= pad;
  hi += pad;
  const x = (t: number) => ((t - (now - span)) / span) * W;
  const y = (p: number) => H - ((p - lo) / (hi - lo)) * H;
  const line = pts.map((q) => `${x(q.t).toFixed(1)},${y(Number(q.p)).toFixed(1)}`).join(" ");
  $("spark-line").setAttribute("points", line);
  const entry = $("spark-entry");
  const lead = $("lead");
  spark.classList.remove("win", "lose");
  lead.classList.remove("win", "lose");
  if (bet && price !== null) {
    const ey = y(Number(bet.openPrice)).toFixed(1);
    entry.setAttribute("y1", ey);
    entry.setAttribute("y2", ey);
    $("spark-area").setAttribute("d", `M0,${ey} L${line.split(" ").join(" L")} L${x(now).toFixed(1)},${ey} Z`);
    const diff = price - bet.openPrice;
    const winning = diff !== 0n && (diff > 0n) === bet.up;
    const by = usd(diff < 0n ? -diff : diff);
    if (diff === 0n) lead.textContent = "Even with your entry";
    else {
      spark.classList.add(winning ? "win" : "lose");
      lead.classList.add(winning ? "win" : "lose");
      lead.textContent = `${winning ? "Winning" : "Losing"} by ${by}`;
    }
    const left = bet.expiry - now / 1000;
    $("bar").hidden = false;
    $("bar").classList.toggle("final", left <= 5);
    $("bar-fill").style.width = `${Math.min(100, Math.max(0, (1 - left / UPDOWN.roundSecs) * 100))}%`;
  } else {
    entry.setAttribute("y1", "-10");
    entry.setAttribute("y2", "-10");
    $("spark-area").setAttribute("d", "");
    lead.textContent = " ";
    $("bar").hidden = true;
  }
}

$("up-btn").onclick = () => placeBet(true);
$("down-btn").onclick = () => placeBet(false);

async function placeBet(up: boolean) {
  const id = liveSession()?.id as string | undefined;
  if (!id) return;
  busy = true;
  render();
  try {
    $("status").textContent = "Placing your bet…";
    await sendTx(er, [cf.bet(session!.keypair.publicKey, potIdOf(id), up)], session!.keypair);
    await readRollup(id);
  } catch (e) {
    showError(e);
    busy = false;
    render();
    return;
  }
  await watchBet(id);
}

/** Counts the open bet down, settles it at expiry (the rollup's closer would too), shows the result. */
let watching = false;
async function watchBet(id: string) {
  if (watching) return;
  watching = true;
  busy = true;
  try {
    const b = lastTable!.bet;
    const winsBefore = lastTable!.wins, tiesBefore = lastTable!.ties, flipsBefore = lastTable!.flips;
    const arrow = b.up ? "▲ Up" : "▼ Down";
    for (;;) {
      const left = b.expiry - Math.floor(Date.now() / 1000);
      $("betline").textContent = `${arrow} from ${usd(b.openPrice)} · ${left > 0 ? `${left} s left` : "settling…"}`;
      $("status").textContent = left > 0 ? "Your bet is in." : "Settling at the next price…";
      render();
      if (left <= 0) break;
      await sleep(1_000);
    }
    // Settle right away: the program takes only a price published within 2 s of the expiry
    // (later, the bet is void). The house and the rollup's closer try too.
    for (let i = 0; i < 30; i++) {
      const r = await readRollup(id).catch(() => null);
      if (r && !r.t.bet.open) break;
      await sendTx(er, [cf.settleBet(potIdOf(id))], session!.keypair).catch(() => {});
      await sleep(300);
    }
    const r = await readRollup(id);
    if (!r) throw new Error("could not read your table");
    seats = r.seats;
    lastResults = resultDots(r.t);
    const t = r.t;
    const end = t.bet.settlePrice;
    if (t.flips === flipsBefore) {
      $("status").textContent = "No price came in time: the bet is void, your stake stays.";
    } else if (t.wins > winsBefore) {
      $("status").innerHTML = `<span class="ok">${arrow}! ${usd(b.openPrice)} → ${usd(end)} · you win +${taz((stake * (UPDOWN.payoutBps - 10_000n)) / 10_000n)}</span>`;
    } else if (t.ties > tiesBefore) {
      $("status").textContent = `Unchanged at ${usd(end)}: your stake is refunded.`;
    } else {
      $("status").textContent = `${b.up ? "Down" : "Up"}: ${usd(b.openPrice)} → ${usd(end)}. The house wins this one.`;
    }
    $("betline").textContent = " ";
    if (game) {
      game.flips = t.flips;
      game.wins = t.wins;
      game.ties = t.ties;
    }
  } catch (e) {
    showError(e);
  } finally {
    watching = false;
    busy = false;
    render();
  }
}

/** Follows the house's record of the session: setup, the rollup seats, closing. */
async function pollGame() {
  const id = localStorage.getItem(K("game"));
  if (!id) return;
  const was = liveSession();
  try {
    game = await house(`/game/${id}`);
  } catch {
    return;
  }
  if (game.state === "in_rollup" && !busy) {
    const r = await readRollup(id).catch(() => null);
    if (r) {
      seats = r.seats;
      tableState = r.t.state;
      lastResults = resultDots(r.t);
      // A bet left open (the page was reloaded): follow it to its result.
      if (r.t.game === 1 && r.t.bet.open && !watching) void watchBet(id);
    }
  }
  const s = liveSession();
  if (was && !s && seats) {
    // Closed and paid back: the seat lands in the ledger balance; show it until a read does.
    pending = { seat: game.state === "failed" ? 0n : seats[1], before: balance, t: Date.now() };
    seats = null;
    lastResults = "";
    void pollBalance();
    void pollHistory();
  }
  if (!busy) {
    if (s && ["finishing", "returned", "cashed_out"].includes(s.state)) $("status").textContent = "Closing your table…";
    else if (s?.state === "in_rollup" && seats && (seats[0] < stake || seats[1] < stake)) $("status").textContent = seats[1] < stake ? "Out of stakes at this table." : "You took all the house's stakes at this table!";
    else if (!s && $("status").textContent === "Closing your table…") $("status").textContent = " ";
  }
  render();
}

// ---------------------------------------------------------------- cash out

const NETWORK_FEE = 20_000n; // the ledger's network-fee allowance per withdrawal
$("cashout-btn").onclick = () => {
  $("withdraw").hidden = !$("withdraw").hidden;
  $("add").hidden = true;
  const all = total();
  const fee = (all * 30n) / 10_000n;
  const net = all - fee - NETWORK_FEE;
  $("w-balance").textContent = taz(all);
  $("w-fee").textContent = taz(fee);
  $("w-net").textContent = all < minExit ? `nothing yet (the smallest withdrawal is ${taz(minExit)})` : net > 0n ? taz(net) : "nothing (below the fees)";
  const a = localStorage.getItem(payoutKey());
  $("w-addr").textContent = a ? `${a.slice(0, 14)}…${a.slice(-6)}` : "your address";
  $("withdraw-form").hidden = !!localStorage.getItem(K("withdraw-live"));
  $<HTMLButtonElement>("withdraw-go").disabled = net <= 0n || all < minExit;
};
$("withdraw-go").onclick = async () => {
  $<HTMLButtonElement>("withdraw-go").disabled = true;
  busy = true;
  render();
  try {
    // A table in play is closed first: its seat goes back to the ledger balance.
    if (liveSession()) {
      $("cashout-tl").innerHTML = '<li class="wait">closing your table first…</li>';
      await house("/stop", { session: session!.keypair.publicKey.toBase58() });
      for (let i = 0; i < 120 && liveSession(); i++) {
        await sleep(1_500);
        await pollGame();
      }
      if (liveSession()) throw new Error("your table is still closing; try again in a minute");
    }
    let w: { id: string; tx: string } | null = null;
    for (let i = 0; i < 10 && !w; i++) {
      w = await house<{ id: string; tx: string }>("/withdraw", { session: session!.keypair.publicKey.toBase58() }).catch((e) => {
        if (i === 9 || !/closing your table|nothing to withdraw/.test(String(e.message))) throw e;
        return null; // the ledger read may lag the cash-out by a few seconds
      });
      if (!w) await sleep(2_000);
    }
    localStorage.setItem(K("withdraw-live"), w!.id);
    await house("/withdraw/sign", { id: w!.id, tx: signB64(w!.tx, session!) });
    $("withdraw-form").hidden = true;
    void pollWithdraw();
    void pollBalance();
  } catch (e) {
    localStorage.removeItem(K("withdraw-live"));
    $<HTMLButtonElement>("withdraw-go").disabled = false;
    $("cashout-tl").innerHTML = "";
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

function tlItem(e: any) {
  const link = e.sig && e.chain === "solana" ? ` <a href="${solTx(e.sig)}" target="_blank" rel="noopener">tx ↗</a>`
    : e.sig && e.chain === "rollup" ? ` <a href="${erTx(e.sig)}" target="_blank" rel="noopener">rollup tx ↗</a>`
    : e.sig && e.chain === "zcash" ? ` <code>${e.sig.slice(0, 16)}…</code> <span class="zconf" data-tx="${e.sig}"></span>` : "";
  return `<li>${esc(e.step)}${link} <span class="muted">${e.t ?? ""}</span></li>`;
}

async function pollWithdraw() {
  const id = localStorage.getItem(K("withdraw-live")) ?? localStorage.getItem(K("withdraw-last"));
  if (!id) return;
  try {
    const w = await house(`/game/${id}`);
    // A withdrawal in progress keeps its panel open with the status.
    if (localStorage.getItem(K("withdraw-live")) && PAGE === "home") {
      $("withdraw").hidden = false;
      $("withdraw-form").hidden = true;
    }
    const tail = w.state === "settled" ? '<li class="wait">the operators are signing the Zcash payment (a few minutes)…</li>'
      : w.state === "paid" ? '<li class="ok">in your wallet ✓</li>' : "";
    $("cashout-tl").innerHTML = w.events.filter((e: any) => !e.step.startsWith("retrying")).map(tlItem).join("") + tail;
    if (["paid", "final", "expired", "failed"].includes(w.state)) {
      localStorage.removeItem(K("withdraw-live"));
      localStorage.setItem(K("withdraw-last"), id);
    }
  } catch {
    /* keep the last view */
  }
  render();
}

// ---------------------------------------------------------------- history

/** A session's result for the player: their seat now against what they put in. */
function netOf(g: any) {
  const d = BigInt(g.amounts[1]) - BigInt(g.rounds) * BigInt(g.stake);
  return d > 0n ? `<span class="ok">+${taz(d)}</span>` : d < 0n ? `−${taz(-d)}` : "even";
}

async function pollHistory() {
  try {
    const gs: any[] = await house(`/games/${session!.keypair.publicKey.toBase58()}`);
    $("hist").innerHTML = gs.map((g) => {
      const t = new Date(g.created).toLocaleString();
      const head = g.kind === "withdraw"
        ? `Cash out ${taz(BigInt(g.stake))} · ${g.state === "paid" || g.state === "final" ? '<span class="ok">paid</span>' : g.state}`
        : g.rounds
          ? `${g.game === "updown" ? "Up/Down" : "Coin flip"} · ${g.flips ?? 0} ${g.game === "updown" ? "bet" : "flip"}${g.flips === 1 ? "" : "s"}, won ${g.wins ?? 0}${g.amounts ? ` · ${netOf(g)}` : ""}${SESSION_LIVE.includes(g.state) ? " · open" : ""}`
          : `Flip · ${g.playerWon === true ? '<span class="ok">won</span>' : g.playerWon === false ? "lost" : g.state}`;
      const evs = g.events.filter((e: any) => !e.step.startsWith("retrying"));
      return `<li><details><summary>${head} <span class="muted">· ${t}</span></summary><ol class="tl">${evs.map(tlItem).join("")}</ol></details></li>`;
    }).join("") || '<li class="muted">no rounds yet</li>';
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

function boot() {
  $("start").hidden = true;
  $("main").hidden = false;
  // Home: deposit, withdraw and the games. A game page: its game, the balance, a link home.
  $("games").hidden = PAGE !== "home";
  $("game-card").hidden = PAGE === "home";
  $("funds-buttons").hidden = PAGE !== "home";
  $("funds-link").hidden = PAGE === "home";
  renderKey();
  renderAddress();
  const link = localStorage.getItem(K("link"));
  const ua = localStorage.getItem(K("ua"));
  if (link && ua) {
    const amt = BigInt(Math.round(Number(new URL(link.replace("zcash:", "zcash://x/")).searchParams.get("amount") ?? "0") * 1e8));
    showPay(link, ua, amt);
  }
  void Promise.all([pollHouse(), pollBalance(), pollDeposits(), pollGame(), pollWithdraw(), pollHistory()]);
  setInterval(pollBalance, 12_000);
  setInterval(pollDeposits, 6_000);
  setInterval(pollGame, 2_000);
  setInterval(pollWithdraw, 5_000);
  setInterval(pollHouse, 30_000);
  setInterval(pollHistory, 30_000);
  setInterval(pollZcashConfs, 20_000);
  if (PAGE === "updown") {
    void pollPrice();
    setInterval(pollPrice, 500);
  }
}

if (session) boot();
else {
  $("start").hidden = false;
  void prfSupported().then((ok) => {
    if (!passkeysAvailable() || ok === false) {
      // This browser says its passkeys can't hold a player key: lead with the backup key.
      $("pk-help").textContent = passkeysAvailable()
        ? "This browser's passkeys can't hold a player key. Use a session key, or open this page in Safari, or in Chrome signed in to Google Password Manager."
        : "This browser has no passkeys. Use a session key.";
      $("pk-help").hidden = false;
      $("nopk-new").classList.add("primary");
      $("pk-new").classList.remove("primary");
    }
  });
}
