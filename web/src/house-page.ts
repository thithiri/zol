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
import { Coinflip, decodeBalance, sendTx } from "./chain";
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

// ---------------------------------------------------------------- the player key

let session: Session | null = readSession(S.app, localStorage);

async function begin(f: () => Promise<Session>) {
  try {
    session = await f();
    boot();
  } catch (e) {
    const msg = String((e as any)?.message ?? e);
    if (msg === NO_PRF_HELP) {
      // Show the advice where the buttons are, and open the backup-key option.
      $("pk-help").textContent = msg;
      $("pk-help").hidden = false;
      (document.querySelector("#start details") as HTMLDetailsElement).open = true;
    } else {
      showError(e);
    }
  }
}
$("pk-new").onclick = () => begin(async () => storeSession(S.app, await createPasskeySeed(S.app), localStorage, "passkey"));
$("pk-login").onclick = () => begin(async () => storeSession(S.app, await seedFromPasskey(S.app), localStorage, "passkey"));
$("nopk-new").onclick = () => begin(async () => storeSession(S.app, crypto.getRandomValues(new Uint8Array(32)), localStorage, "backup"));
$("restore-go").onclick = () => begin(async () => restoreSession(S.app, $<HTMLInputElement>("restore-text").value, localStorage));
$("logout-btn").onclick = () => {
  if (!confirm("Switch player? You can come back with your passkey, or with this player's backup string.")) return;
  localStorage.removeItem(`zol:session:${S.app}`);
  location.reload();
};
$("backup-btn").onclick = () => {
  $("backup-text").textContent = exportSession(S.app, session!);
  $("backup-out").hidden = false;
};
$("backup-done").onclick = () => {
  markBackedUp(S.app, session!, localStorage);
  $("backup-out").hidden = true;
  renderKey();
};
function renderKey() {
  const kind = sessionKind(S.app, localStorage);
  const ok = isBackedUp(S.app, session!, localStorage);
  $("key-kind").innerHTML = kind === "passkey" ? "held by your passkey" : ok ? "backup string (saved)" : '<span class="err">backup string NOT saved yet</span>';
  $("backup-btn").hidden = kind === "passkey";
}

// ---------------------------------------------------------------- state

const K = (k: string) => `zol:house:${k}:${session!.keypair.publicKey.toBase58()}`;
let app: AppInfo | null = null;
let stake = 0n;
let balance = 0n;
let exiting = false;
let game: any = null;
let busy = false;

async function pollBalance() {
  try {
    const b = await base.getAccountInfo(cf.pdas.balance(session!.keypair.publicKey));
    const bal = b ? decodeBalance(b.data) : null;
    balance = bal?.amount ?? 0n;
    exiting = !!bal?.exiting;
  } catch {
    /* rate limited: keep the last value */
  }
  render();
}
async function pollHouse() {
  try {
    const i = await house("/info");
    stake = BigInt(i.stake);
    $("stake-note").textContent = i.can_play ? `Stake ${taz(stake)} a flip; win ${taz(2n * stake)}` : BigInt(i.bankroll) < stake ? "The house is out of bankroll for now" : "The house is busy; try in a minute";
  } catch {
    // Usually a rate-limited RPC on the operators' side: keep the last note, retry soon.
    if (!stake) $("stake-note").textContent = "Connecting to the house…";
    setTimeout(() => void pollHouse(), 5_000);
  }
  render();
}

function render() {
  $("balance").textContent = taz(balance);
  const n = stake > 0n ? balance / stake : 0n;
  $("flips").textContent = stake > 0n ? (n > 0n ? `${n} flip${n === 1n ? "" : "s"}` : "add funds to play") : " ";
  const playing = busy || (game && game.kind !== "withdraw" && !["done", "final", "paid", "expired", "failed"].includes(game.state));
  for (const id of ["heads", "tails"]) $<HTMLButtonElement>(id).disabled = playing || balance < stake || stake === 0n || exiting;
  $<HTMLButtonElement>("cashout-btn").disabled = playing || balance === 0n || !!localStorage.getItem(K("withdraw-live"));
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

async function deposit(amount: bigint) {
  try {
    if (sessionKind(S.app, localStorage) === "backup" && !isBackedUp(S.app, session!, localStorage)) {
      $("backup-btn").click();
      throw new Error("Save your backup string first (below), then add funds");
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
    const d = await prepareDeposit([gw], S.app, session!, addr, amount, false);
    localStorage.setItem(K("since"), String(d.app.tip));
    localStorage.setItem(K("link"), d.link);
    localStorage.setItem(K("ua"), d.session.ua);
    showPay(d.link, d.session.ua, amount);
    renderAddress();
  } catch (e) {
    showError(e);
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
  $("fund-btn").hidden = amount !== 1_000_000n || !!localStorage.getItem(K("fundtx"));
}
$("fund-btn").onclick = async () => {
  try {
    const link = localStorage.getItem(K("link")) ?? "";
    // The browser Buffer polyfill has no "base64url": map it to standard base64 first.
    const m = new URL(link.replace("zcash:", "zcash://x/")).searchParams.get("memo") ?? "";
    const memo = Buffer.from(m.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
    const r = await house<{ txid: string }>("/fund", { session: hex(session!.publicKey), ...(memo.startsWith("ZOL1:") ? { memo } : {}) });
    localStorage.setItem(K("fundtx"), r.txid);
    $("fund-btn").hidden = true;
    $("fund-msg").hidden = false;
    $("fund-msg").innerHTML = '<span class="ok">Sent: 0.01 TAZ is on its way to your address.</span>';
    void pollDeposits();
  } catch (e) {
    // Say why right here (most often: the once-a-day limit for this network address).
    $("fund-msg").hidden = false;
    $("fund-msg").innerHTML = `<span class="err">${esc(String((e as any)?.message ?? e).split("\n")[0])}.</span> Get testnet ZEC from <a href="https://faucet.testnet.valargroup.dev" target="_blank" rel="noopener">Valar's faucet</a> and pay the QR code instead.`;
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

async function play(c: "H" | "T") {
  call = c;
  busy = true;
  render();
  coin.textContent = c;
  coin.classList.add("spin");
  $("status").textContent = "The house matches your stake…";
  try {
    const p = await house<{ game: string; tx: string }>("/play", { session: session!.keypair.publicKey.toBase58() });
    localStorage.setItem(K("game"), p.game);
    localStorage.setItem(K(`call:${p.game}`), c);
    game = await house("/join", { game: p.game, tx: signB64(p.tx, session!) });
    $("status").textContent = "Moving the table into the rollup…";
  } catch (e) {
    coin.classList.remove("spin");
    $("status").textContent = " ";
    showError(e);
  } finally {
    busy = false;
    render();
  }
}

let flipping = false;
async function pollGame() {
  const id = localStorage.getItem(K("game"));
  if (!id) return;
  try {
    game = await house(`/game/${id}`);
  } catch {
    return;
  }
  call = (localStorage.getItem(K(`call:${id}`)) as "H" | "T") ?? call;
  const st = game.state;
  if (st === "in_rollup" && !flipping && !localStorage.getItem(K(`flipped:${id}`))) {
    flipping = true;
    $("status").textContent = "Flipping…";
    coin.classList.add("spin");
    try {
      const sig = await sendTx(er, [cf.flip(session!.keypair.publicKey, Uint8Array.from(Buffer.from(id, "hex")), new PublicKey(S.vrfQueue), crypto.getRandomValues(new Uint8Array(32)))], session!.keypair);
      localStorage.setItem(K(`flipped:${id}`), JSON.stringify({ sig, t: new Date().toISOString().slice(11, 19) }));
    } catch (e) {
      showError(e); // the house flips after a minute anyway
    } finally {
      flipping = false;
    }
  }
  if (game.playerWon !== undefined) {
    coin.classList.remove("spin");
    const other = call === "H" ? "T" : "H";
    coin.textContent = game.playerWon ? call : other;
    $("status").innerHTML = game.playerWon
      ? `<span class="ok">${call === "H" ? "Heads" : "Tails"}! You win ${taz(2n * BigInt(game.stake))}</span>`
      : `${other === "H" ? "Heads" : "Tails"}. The house wins this one.`;
    if (["done", "final", "paid"].includes(st)) {
      if (!localStorage.getItem(K(`seen:${id}`))) {
        localStorage.setItem(K(`seen:${id}`), "1");
        void pollBalance();
        void pollHistory();
      }
    } else if (game.playerWon) {
      $("status").innerHTML += ' <span class="muted small">(adding to your balance…)</span>';
    }
  } else if (["expired", "failed"].includes(st)) {
    coin.classList.remove("spin");
    $("status").innerHTML = `<span class="err">${esc(String(game.error ?? st).split("\n")[0])}</span>`;
  } else if (st !== "in_rollup") {
    coin.classList.add("spin");
  }
  render();
}

// ---------------------------------------------------------------- cash out

$("cashout-btn").onclick = async () => {
  if (!confirm(`Cash out ${taz(balance)} to your Zcash address? (Less the 0.3% exit fee and a 0.0002 TAZ network fee.)`)) return;
  try {
    const w = await house<{ id: string; tx: string }>("/withdraw", { session: session!.keypair.publicKey.toBase58() });
    localStorage.setItem(K("withdraw-live"), w.id);
    await house("/withdraw/sign", { id: w.id, tx: signB64(w.tx, session!) });
    void pollWithdraw();
    void pollBalance();
  } catch (e) {
    localStorage.removeItem(K("withdraw-live"));
    showError(e);
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
    $("cashout-status").hidden = false;
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

async function pollHistory() {
  try {
    const gs: any[] = await house(`/games/${session!.keypair.publicKey.toBase58()}`);
    $("hist").innerHTML = gs.map((g) => {
      const t = new Date(g.created).toLocaleString();
      const head = g.kind === "withdraw"
        ? `Cash out ${taz(BigInt(g.stake))} · ${g.state === "paid" || g.state === "final" ? '<span class="ok">paid</span>' : g.state}`
        : `Flip · ${g.playerWon === true ? '<span class="ok">won</span>' : g.playerWon === false ? "lost" : g.state}`;
      const flip = localStorage.getItem(K(`flipped:${g.id}`));
      const f = flip && flip.startsWith("{") ? JSON.parse(flip) : null;
      const evs = g.events.filter((e: any) => !e.step.startsWith("retrying"));
      if (f) evs.splice(evs.findIndex((e: any) => e.step.startsWith("table in the")) + 1, 0, { step: "you flipped the coin", t: f.t, chain: "rollup", sig: f.sig });
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
}

if (session) boot();
else {
  $("start").hidden = false;
  void prfSupported().then((ok) => {
    if (!passkeysAvailable() || ok === false) {
      // This browser says its passkeys can't hold a player key: lead with the backup key.
      $("pk-help").textContent = passkeysAvailable()
        ? "This browser's passkeys can't hold a player key. Use a backup key, or open this page in Safari, or in Chrome signed in to Google Password Manager."
        : "This browser has no passkeys. Use a backup key.";
      $("pk-help").hidden = false;
      (document.querySelector("#start details") as HTMLDetailsElement).open = true;
    }
  });
}
