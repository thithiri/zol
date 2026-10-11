// The sealed-bid auction page: register (deposit), seal a bid in the rollup, reveal it, see the
// result. Same player key and ledger balance as the game page; the house is the seller and
// pays the Solana fees (web/house/auction.ts).
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import { Auctions, bidCommitment, bidSalt, Coinflip, decodeBalance, sendTx } from "./chain";
import { readSession } from "./session";

const env = (import.meta as any).env ?? {};
const S = {
  baseRpc: env.VITE_BASE_RPC ?? "http://127.0.0.1:28899",
  erRpc: env.VITE_ER_RPC ?? "http://127.0.0.1:27799",
  ledger: env.VITE_LEDGER ?? "3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y",
  app: env.VITE_APP ?? "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo",
  house: env.VITE_HOUSE ?? "/house",
};
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const base = new Connection(S.baseRpc, "confirmed");
const er = new Connection(S.erRpc, "confirmed");
const cf = new Coinflip({ ledger: new PublicKey(S.ledger), app: new PublicKey(S.app) });
const au = new Auctions(cf);
const taz = (zat: number | bigint) => `${(Number(zat) / 1e8).toFixed(8).replace(/0+$/, "").replace(/\.$/, "")} TAZ`;
const zatOf = (s: string) => BigInt(Math.round(Number(s.trim()) * 1e8));
const short = (k: string) => `${k.slice(0, 4)}…${k.slice(-4)}`;

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

const session = readSession(S.app, localStorage);
const me = session?.keypair.publicKey.toBase58() ?? "";
let a: any = null; // the house's record of the current auction, with `chain`
let balance = 0n;
let busy = false;
let revealing = false;

/** This browser remembers the amount it sealed, to reveal it. */
const bidKey = (auction: string) => `zol:auction:bid:${auction}:${me}`;
const storedBid = (auction: string) => { try { const v = localStorage.getItem(bidKey(auction)); return v ? BigInt(v) : null; } catch { return null; } };

async function poll() {
  try {
    a = (await house("/auction")).auction;
  } catch {
    return;
  }
  render();
  void autoReveal();
}
async function pollBalance() {
  try {
    const b = await base.getAccountInfo(cf.pdas.balance(session!.keypair.publicKey));
    balance = b ? decodeBalance(b.data).amount : 0n;
    $("balance").textContent = taz(balance);
  } catch {
    /* rate limited: keep the last value */
  }
}

function phaseOf() {
  const c = a?.chain;
  if (!a || !c) return a ? (a.state === "cancelled" ? "cancelled" : "none") : "none";
  if (c.state === "Ended" || a.state === "done") return "ended";
  if (a.state === "cancelled") return "cancelled";
  const t = Date.now() / 1000;
  if (c.state === "Registering") return t < c.registerDeadline ? "register" : "starting";
  if (t < c.commitDeadline) return "bid";
  if (t < c.revealDeadline) return "reveal";
  return "ending";
}

function clock(until: number) {
  const s = Math.max(0, Math.round(until - Date.now() / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function render() {
  const c = a?.chain;
  const ph = phaseOf();
  const seat: number = c ? c.players.indexOf(me) : -1;
  const registered = seat >= 1;
  const committed = registered && (c.committed & (1 << seat)) !== 0;
  const revealed = registered && (c.revealed & (1 << seat)) !== 0;
  $("title").textContent = a ? `Certificate #${a.number}` : "No auction yet";
  $("deposit").textContent = c ? taz(BigInt(c.deposit)) : "—";
  $("bidders").textContent = c ? `${c.seatCount - 1} of 8` : "—";
  for (const [id, on] of [["s-reg", ph === "register" || ph === "starting"], ["s-bid", ph === "bid"], ["s-rev", ph === "reveal"], ["s-end", ph === "ending" || ph === "ended"]] as const) {
    $(id).classList.toggle("on", on);
  }
  const phase: Record<string, [string, number | null]> = {
    none: ["No auction is running.", null],
    cancelled: ["Nobody registered, so the auction was cancelled.", null],
    register: ["Registration closes in", c?.registerDeadline],
    starting: ["Moving the auction into the rollup…", null],
    bid: ["Bidding closes in", c?.commitDeadline],
    reveal: ["Reveal closes in", c?.revealDeadline],
    ending: ["The rollup is ending the auction…", null],
    ended: ["Auction over.", null],
  };
  const [text, until] = phase[ph];
  $("phase").textContent = text;
  $("clock").textContent = until ? clock(until) : " ";

  $("open-btn").hidden = !["none", "cancelled", "ended"].includes(ph);
  $<HTMLButtonElement>("open-btn").disabled = busy;
  $("register-btn").hidden = !(ph === "register" && !registered);
  $("register-btn").textContent = c ? `Register: deposit ${taz(BigInt(c.deposit))}` : "Register";
  $<HTMLButtonElement>("register-btn").disabled = busy || !c || balance < BigInt(c.deposit) || c.seatCount >= 9;
  $("bid-box").hidden = !(ph === "bid" && registered);
  $<HTMLButtonElement>("seal-btn").disabled = busy;
  const sealed = a ? storedBid(a.id) : null;
  $("reveal-box").hidden = !(ph === "reveal" && committed && !revealed && sealed === null);
  $<HTMLButtonElement>("reveal-btn").disabled = busy;

  let mine = " ";
  if (registered) {
    if (ph === "register" || ph === "starting") mine = "You're registered. Bidding opens when registration closes.";
    else if (ph === "bid") mine = committed ? `Your sealed bid: ${sealed !== null ? taz(sealed) : "set from another device"} (only you know it)` : "Seal your bid before bidding closes.";
    else if (ph === "reveal") mine = revealed ? `Revealed: ${taz(BigInt(c.bids?.[seat] ?? sealed ?? 0))} ✓` : committed ? (sealed !== null ? "Revealing your bid…" : "Enter the bid you sealed to reveal it.") : "You didn't bid; your deposit comes back.";
  } else if (c && ph !== "none" && ph !== "register") {
    mine = "You're not in this auction.";
  }
  if (ph === "ended" && c) {
    const won = c.winnerKey === me;
    if (won) mine = `🎉 You won certificate #${a.number} for ${taz(BigInt(c.price))}.`;
    else if (registered) mine = committed && !revealed ? "You didn't reveal: 10% of your deposit went to those who did." : "You didn't win; your deposit is back in your balance.";
  }
  $("mine").textContent = mine;

  const result: string[] = [];
  if (ph === "ended" && c) {
    result.push(c.winnerKey ? `Winner <code>${short(c.winnerKey)}</code> paid <b>${taz(BigInt(c.price))}</b> (first price).` : "No bid was revealed: no sale.");
    result.push(`Certificate on Solana: <a href="https://explorer.solana.com/address/${a.address}?cluster=devnet" target="_blank" rel="noopener">${short(a.address)} ↗</a>`);
  }
  if (a?.events?.length) result.push(`<span class="muted">${a.events.map((e: any) => `${e.t} ${e.step}`).join("<br>")}</span>`);
  $("result").innerHTML = result.join("<br>");
}

$("open-btn").onclick = async () => {
  busy = true;
  render();
  try {
    a = (await house("/auction/open", {})).auction;
  } catch (e) {
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

$("register-btn").onclick = async () => {
  busy = true;
  render();
  try {
    const r = await house<{ id: string; tx: string }>("/auction/register", { session: me });
    const tx = Transaction.from(Buffer.from(r.tx, "base64"));
    tx.partialSign(session!.keypair);
    await house("/auction/register/sign", { id: r.id, tx: Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64") });
    await poll();
    void pollBalance();
  } catch (e) {
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

$("seal-btn").onclick = async () => {
  const c = a?.chain;
  if (!c) return;
  const amount = zatOf($<HTMLInputElement>("bid-input").value);
  if (!(amount > 0n) || amount > BigInt(c.deposit)) return showError(`a bid is more than 0 and at most ${taz(BigInt(c.deposit))}`);
  busy = true;
  render();
  try {
    const potId = Buffer.from(a.id, "hex");
    const auction = au.auction(potId);
    const salt = bidSalt(session!.seed, auction);
    await sendTx(er, [au.commit(session!.keypair.publicKey, potId, bidCommitment(auction, session!.keypair.publicKey, amount, salt))], session!.keypair);
    localStorage.setItem(bidKey(a.id), String(amount));
    $<HTMLInputElement>("bid-input").value = "";
    await poll();
  } catch (e) {
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

async function reveal(amount: bigint) {
  const potId = Buffer.from(a.id, "hex");
  const salt = bidSalt(session!.seed, au.auction(potId));
  await sendTx(er, [au.reveal(session!.keypair.publicKey, potId, amount, salt)], session!.keypair);
}

/** In the reveal window, reveal the bid this browser sealed, once. */
async function autoReveal() {
  const c = a?.chain;
  if (!c || revealing || phaseOf() !== "reveal") return;
  const seat = c.players.indexOf(me);
  if (seat < 1 || !(c.committed & (1 << seat)) || c.revealed & (1 << seat)) return;
  const amount = storedBid(a.id);
  if (amount === null) return;
  revealing = true;
  try {
    await reveal(amount);
    await poll();
  } catch (e) {
    showError(e);
  } finally {
    revealing = false;
  }
}

$("reveal-btn").onclick = async () => {
  busy = true;
  render();
  try {
    await reveal(zatOf($<HTMLInputElement>("reveal-input").value));
    await poll();
  } catch (e) {
    showError(e);
  } finally {
    busy = false;
    render();
  }
};

if (!session) {
  $("nosession").hidden = false;
} else {
  $("main").hidden = false;
  $("me").textContent = me;
  void poll();
  void pollBalance();
  setInterval(poll, 2_000);
  setInterval(pollBalance, 12_000);
  setInterval(() => !busy && render(), 1_000);
}
