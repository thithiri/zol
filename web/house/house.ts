// The house: an automated second player for single-player coin flips (docs/plan-house-mode.md).
//
// The house holds a session key with a ledger balance (the bankroll) and a Solana fee payer.
// For a player who asks to play, it builds ONE transaction that creates a table and seats
// the house and the player; the player signs it, the house adds its signatures and sends it.
// Either both are seated or nothing happens. The house then drives the game to the end
// (start, flip if the player does not, finish, cash out, close, settle the player's
// auto-exited balance) and follows the payout on Zcash until it is final. It pays every
// Solana fee; rollup transactions are free.
//
// No program changes: to the ledger and the VRF the house is an ordinary player.
import { Connection, Keypair, PublicKey, Transaction } from "@solana/web3.js";
import {
  allowedStakes, Coinflip, decodeApp, decodeBalance, decodeLedgerParams, decodeReceipt, decodeTable, DLP, sendSigned, sendTx,
} from "../src/chain";
import { hex, sessionFromSeed, unhex, type Session } from "../src/session";

export interface HouseConfig {
  baseRpc: string;
  erRpc: string;
  ledger: string;
  app: string;
  vrfQueue: string;
  validator: string;
  /** This operator's page API and zol-zcashd (local), and the Zcash node. */
  gateway: string;
  zcashd: string;
  zcashNode: string;
  genesisHead: string; // hex
  maxOpenGames: number;
  /** A player who has not flipped this long after the table reached the rollup: the house flips. */
  flipAfterSecs: number;
  fundZat: number;
  fundPerDay: number;
  fundPerIpPerDay: number;
  playsPerIpPerHour: number;
}

export type GameState =
  | "awaiting_signature" | "seated" | "started" | "in_rollup" | "landed" | "finishing" | "returned"
  | "cashed_out" | "settled" | "paid" | "final" | "done" | "expired" | "failed";

export interface GameEvent { step: string; t: string; chain?: "solana" | "rollup" | "zcash"; sig?: string; detail?: string }

export interface Game {
  id: string; // pot id hex
  player: string; // session, base58
  stake: string;
  state: GameState;
  created: number;
  updated: number;
  events: GameEvent[];
  /** The unsigned create+join transaction, base64, and its blockhash expiry. */
  tx?: string;
  lastValid?: number;
  inRollupAt?: number;
  /** Seat 0 is the house, seat 1 the player. */
  winner?: number;
  playerWon?: boolean;
  settlement?: string;
  zcash?: { txid: string; height: number };
  error?: string;
}

/** Where games and counters live: SQLite in the service, a Map in tests. */
export interface Store {
  get(id: string): Game | undefined;
  put(g: Game): void;
  active(): Game[];
  byPlayer(player: string): Game[];
  count(key: string, sinceMs: number): number;
  bump(key: string): void;
}

export class MemoryStore implements Store {
  games = new Map<string, Game>();
  hits: { key: string; t: number }[] = [];
  get(id: string) { return this.games.get(id); }
  put(g: Game) { this.games.set(g.id, structuredClone(g)); }
  active() { return [...this.games.values()].filter((g) => !TERMINAL.includes(g.state)); }
  byPlayer(p: string) { return [...this.games.values()].filter((g) => g.player === p); }
  count(key: string, since: number) { return this.hits.filter((h) => h.key === key && h.t >= since).length; }
  bump(key: string) { this.hits.push({ key, t: Date.now() }); }
}

export const TERMINAL: GameState[] = ["final", "done", "expired", "failed"];
/** States in which the player's stake is still committed to the game. */
const LIVE: GameState[] = ["awaiting_signature", "seated", "started", "in_rollup", "landed", "finishing", "returned", "cashed_out"];

export class HouseError extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}

const now = () => new Date().toISOString().slice(11, 19);

export class House {
  readonly cf: Coinflip;
  readonly base: Connection;
  readonly er: Connection;
  private busy = new Set<string>();

  constructor(
    readonly cfg: HouseConfig,
    readonly session: Session,
    readonly feePayer: Keypair,
    readonly store: Store,
    readonly http: typeof fetch = fetch,
  ) {
    this.cf = new Coinflip({ ledger: new PublicKey(cfg.ledger), app: new PublicKey(cfg.app) });
    this.base = new Connection(cfg.baseRpc, "confirmed");
    this.er = new Connection(cfg.erRpc, "confirmed");
  }

  static sessionFromSeedHex(seedHex: string) {
    return sessionFromSeed(unhex(seedHex));
  }

  // ------------------------------------------------------------ reads

  private stakeCache?: { v: bigint; t: number };
  private infoCache?: { v: Awaited<ReturnType<House["computeInfo"]>>; t: number };

  /** The stake follows the ledger's parameters, which rarely change: cached 10 min. */
  async stake(): Promise<bigint> {
    if (this.stakeCache && Date.now() - this.stakeCache.t < 600_000) return this.stakeCache.v;
    const c = await this.base.getAccountInfo(this.cf.pdas.config());
    const v = allowedStakes(decodeLedgerParams(c!.data))[0];
    this.stakeCache = { v, t: Date.now() };
    return v;
  }

  async balanceOf(session: PublicKey) {
    const b = await this.base.getAccountInfo(this.cf.pdas.balance(session));
    return b ? decodeBalance(b.data) : null;
  }

  /** Every visitor's page polls this: cached 10 s to spare the RPC (one IP, rate limited). */
  async info() {
    if (this.infoCache && Date.now() - this.infoCache.t < 10_000) return this.infoCache.v;
    const v = await this.computeInfo();
    this.infoCache = { v, t: Date.now() };
    return v;
  }

  private async computeInfo() {
    const stake = await this.stake();
    const bal = await this.balanceOf(this.session.keypair.publicKey);
    const open = this.store.active().filter((g) => LIVE.includes(g.state)).length;
    const bankroll = bal?.amount ?? 0n;
    const day = Date.now() - 86_400_000;
    return {
      house: this.session.keypair.publicKey.toBase58(),
      stake: String(stake),
      bankroll: String(bankroll),
      open_games: open,
      max_open_games: this.cfg.maxOpenGames,
      can_play: bankroll >= stake && open < this.cfg.maxOpenGames,
      fund_me: { zat: this.cfg.fundZat, left_today: Math.max(0, this.cfg.fundPerDay - this.store.count("fund", day)) },
      fee_payer_sol: (await this.base.getBalance(this.feePayer.publicKey)) / 1e9,
    };
  }

  game(id: string) {
    const g = this.store.get(id);
    if (!g) throw new HouseError(404, "no such game");
    const { tx, ...rest } = g;
    return rest;
  }

  gamesOf(player: string) {
    return this.store.byPlayer(player).map(({ tx, ...g }) => g).sort((a, b) => b.created - a.created).slice(0, 10);
  }

  // ------------------------------------------------------------ play

  /**
   * Builds the create+join transaction for `player` (base58 session). The player signs it
   * and returns it through `join`. The house only checks what it can: the player's balance
   * covers the stake and the player has no other live game.
   */
  async play(player: string, ip: string) {
    if (this.store.count(`play:${ip}`, Date.now() - 3_600_000) >= this.cfg.playsPerIpPerHour) throw new HouseError(429, "too many games from this address; try again later");
    let who: PublicKey;
    try { who = new PublicKey(player); } catch { throw new HouseError(400, "session must be a base58 public key"); }
    if (who.equals(this.session.keypair.publicKey)) throw new HouseError(400, "that is the house");
    const live = this.store.byPlayer(player).find((g) => LIVE.includes(g.state) && !(g.state === "awaiting_signature" && this.expired(g)));
    if (live) throw new HouseError(409, `you already have a game in progress (${live.id.slice(0, 8)}…)`);
    const stake = await this.stake();
    const bal = await this.balanceOf(who);
    if (!bal || bal.amount < stake) throw new HouseError(409, `your ledger balance must be at least ${stake} zat; deposit first`);
    if (bal.exiting) throw new HouseError(409, "your balance is being paid out; deposit again to play");
    const info = await this.computeInfo();
    if (!info.can_play) throw new HouseError(503, BigInt(info.bankroll) < stake ? "the house is out of bankroll; try later" : "the house is busy; try again in a minute");

    const potId = crypto.getRandomValues(new Uint8Array(32));
    const ixs = [
      this.cf.createTable(this.feePayer.publicKey, potId, stake, 600n, new PublicKey(this.cfg.validator)),
      this.cf.join(this.session.keypair.publicKey, potId), // seat 0: the house
      this.cf.join(who, potId), // seat 1: the player
    ];
    const tx = new Transaction().add(...ixs);
    tx.feePayer = this.feePayer.publicKey;
    const bh = await this.base.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash;
    const unsigned = tx.serialize({ requireAllSignatures: false, verifySignatures: false });
    const g: Game = {
      id: hex(potId), player, stake: String(stake), state: "awaiting_signature", created: Date.now(), updated: Date.now(),
      events: [{ step: "table prepared: the house matches your stake", t: now() }],
      tx: Buffer.from(unsigned).toString("base64"), lastValid: bh.lastValidBlockHeight,
    };
    this.store.put(g);
    this.store.bump(`play:${ip}`);
    return { game: g.id, tx: g.tx, table: this.cf.pdas.table(potId).toBase58(), stake: g.stake, house: info.house };
  }

  private expired(g: Game) {
    return Date.now() - g.created > 90_000; // a blockhash lives about 60-90 s
  }

  /**
   * The player's signed create+join. It must be exactly the transaction the house built,
   * signed by the player's session; the house then adds its own two signatures.
   */
  async join(id: string, signedB64: string) {
    const g = this.store.get(id);
    if (!g) throw new HouseError(404, "no such game");
    if (g.state !== "awaiting_signature") throw new HouseError(409, `game is ${g.state}`);
    const tx = checkSigned(g.tx!, signedB64, new PublicKey(g.player));
    tx.partialSign(this.feePayer, this.session.keypair);
    try {
      const sig = await sendSigned(this.base, tx.serialize(), g.lastValid!);
      g.state = "seated";
      g.events.push({ step: "table created; you and the house are seated", t: now(), chain: "solana", sig });
    } catch (e: any) {
      g.state = /expired/.test(String(e.message)) ? "expired" : "failed";
      g.error = String(e.message).slice(0, 400);
      g.events.push({ step: g.state === "expired" ? "took too long to sign; start again" : "could not create the table", t: now(), detail: g.error });
    }
    g.updated = Date.now();
    this.store.put(g);
    if (g.state !== "seated") throw new HouseError(409, g.error ?? g.state);
    return this.game(id);
  }

  // ------------------------------------------------------------ driving games

  /** Advances every live game by at most one step. Call every few seconds. */
  async tick() {
    for (const g of this.store.active()) {
      if (this.busy.has(g.id)) continue;
      this.busy.add(g.id);
      try {
        await this.step(g);
      } catch (e: any) {
        g.error = String(e?.message ?? e).slice(0, 400);
        g.events.push({ step: `retrying: ${g.error.split("\n")[0].slice(0, 120)}`, t: now() });
        if (g.events.filter((x) => x.step.startsWith("retrying")).length > 8) {
          g.state = "failed";
          g.events.push({ step: "the house gave up on this game; an operator will refund it", t: now() });
        }
      } finally {
        g.updated = Date.now();
        this.store.put(g);
        this.busy.delete(g.id);
      }
    }
  }

  private async step(g: Game) {
    const potId = unhex(g.id);
    const table = this.cf.pdas.table(potId);
    const pot = this.cf.pdas.pot(potId);
    const house = this.session.keypair;
    switch (g.state) {
      case "awaiting_signature":
        if (this.expired(g)) {
          g.state = "expired";
          g.events.push({ step: "not signed in time; nothing was created", t: now() });
        }
        return;
      case "seated": {
        const sig = await sendTx(this.base, [this.cf.start(this.feePayer.publicKey, potId, 600n)], this.feePayer, [], 400_000);
        g.state = "started";
        g.events.push({ step: "game started: the table moves into the rollup", t: now(), chain: "solana", sig });
        return;
      }
      case "started": {
        if ((await this.base.getAccountInfo(table))?.owner.equals(DLP)) {
          g.state = "in_rollup";
          g.inRollupAt = Date.now();
          g.events.push({ step: "table in the MagicBlock rollup: flip the coin", t: now(), chain: "rollup" });
        }
        return;
      }
      case "in_rollup": {
        const t = decodeTable((await this.er.getAccountInfo(table))!.data);
        if (t.state === "Landed") return this.landed(g, t.winner);
        if (t.state === "Started" && Date.now() - (g.inRollupAt ?? 0) > this.cfg.flipAfterSecs * 1000) {
          const sig = await sendTx(this.er, [this.cf.flip(house.publicKey, potId, new PublicKey(this.cfg.vrfQueue), crypto.getRandomValues(new Uint8Array(32)))], house);
          g.events.push({ step: "you did not flip, so the house flipped for you (the VRF decides either way)", t: now(), chain: "rollup", sig });
          g.inRollupAt = Date.now() + 3_600_000; // once
        }
        return;
      }
      case "landed": {
        const sig = await sendTx(this.er, [this.cf.finish(house.publicKey, potId)], house);
        g.state = "finishing";
        g.events.push({ step: "game finished in the rollup; the pot returns to Solana", t: now(), chain: "rollup", sig });
        return;
      }
      case "finishing": {
        if ((await this.base.getAccountInfo(pot))?.owner.equals(this.cf.p.ledger)) {
          g.state = "returned";
          g.events.push({ step: "pot back on Solana", t: now(), chain: "solana" });
        }
        return;
      }
      case "returned": {
        const r = decodeReceipt((await this.base.getAccountInfo(this.cf.pdas.receipt(pot)))!.data);
        const sig = await sendTx(this.base, [this.cf.cashOut(potId, r.players)], this.feePayer);
        g.state = "cashed_out";
        g.events.push({ step: g.playerWon ? "winnings credited to your ledger balance" : "pot paid to the house", t: now(), chain: "solana", sig });
        return;
      }
      case "cashed_out": {
        // Return the table's rent to the house, then settle the player's balance if it auto-exited.
        if (await this.base.getAccountInfo(table)) {
          await sendTx(this.base, [this.cf.closeTable(potId, this.feePayer.publicKey)], this.feePayer).catch(() => {});
        }
        const me = new PublicKey(g.player);
        const bal = await this.balanceOf(me);
        if (!bal || bal.amount === 0n) {
          g.state = "done";
          g.events.push({ step: "nothing to pay out", t: now() });
          return;
        }
        if (!bal.exiting) {
          g.state = "done";
          g.events.push({ step: `your balance (${bal.amount} zat) stays on the ledger: use "Exit and settle" to be paid`, t: now() });
          return;
        }
        const app = decodeApp((await this.base.getAccountInfo(this.cf.pdas.app()))!.data);
        const sig = await sendTx(this.base, [this.cf.settle(this.feePayer.publicKey, app.nextSettlement, [me])], this.feePayer);
        g.settlement = String(app.nextSettlement);
        g.state = "settled";
        g.events.push({ step: `settlement ${app.nextSettlement}: the operators pay ${bal.amount} zat less fees to your Zcash address`, t: now(), chain: "solana", sig });
        return;
      }
      case "settled": {
        const head = await this.json(`${this.cfg.zcashd}/head?app=${hex(this.cf.p.app.toBytes())}&genesis=${this.cfg.genesisHead}`);
        const link = head.chain?.find((l: any) => l.paid_count > Number(g.settlement));
        if (link) {
          const txid = Buffer.from(link.txid).reverse().toString("hex");
          g.zcash = { txid, height: link.height };
          g.state = "paid";
          g.events.push({ step: "paid on Zcash: it is in your wallet", t: now(), chain: "zcash", sig: txid, detail: `block ${link.height}` });
        }
        return;
      }
      case "paid": {
        // Final once the ledger marks the settlement paid (payout depth); poll slowly.
        if (Date.now() - g.updated < 60_000) return;
        const app = decodeApp((await this.base.getAccountInfo(this.cf.pdas.app()))!.data);
        if (app.paidCount > BigInt(g.settlement!)) {
          g.state = "final";
          g.events.push({ step: "final: the payout is deep on Zcash and marked paid on Solana", t: now(), chain: "solana" });
        }
        return;
      }
    }
  }

  private landed(g: Game, winner: number) {
    g.winner = winner;
    g.playerWon = winner === 1;
    g.state = "landed";
    g.events.push({ step: g.playerWon ? "the coin landed: you won" : "the coin landed: the house won", t: now(), chain: "rollup", detail: `seat ${winner}` });
  }

  // ------------------------------------------------------------ fund me, Zcash lookups

  /**
   * Sends one deposit from the house's funding wallet to the player's session address
   * (rate limited): lets someone without a testnet wallet try the flow. The page must have
   * registered the deposit intent first, so the deposit is credited without a memo.
   */
  async fund(sessionHex: string, ip: string, memo?: string) {
    if (!/^[0-9a-f]{64}$/.test(sessionHex)) throw new HouseError(400, "session must be 32-byte hex");
    const day = Date.now() - 86_400_000;
    if (this.store.count(`fund:${sessionHex}`, 0) > 0) throw new HouseError(429, "this session was already funded once");
    if (this.store.count(`fund-ip:${ip}`, day) >= this.cfg.fundPerIpPerDay) throw new HouseError(429, "funding limit for your address reached today");
    if (this.store.count("fund", day) >= this.cfg.fundPerDay) throw new HouseError(429, "the house's daily funding is used up; use a testnet faucet");
    if (memo !== undefined && (!memo.startsWith("ZOL1:") || memo.length > 512)) throw new HouseError(400, "memo must be the ZOL1 text from your deposit link");
    const s = await this.json(`${this.cfg.gateway}/v1/apps/${this.cfg.app}/sessions/${sessionHex}`);
    if (!s.intent_registered) throw new HouseError(409, "create your deposit link first");
    this.store.bump(`fund:${sessionHex}`); // count before sending: a failure must not allow a retry storm
    this.store.bump(`fund-ip:${ip}`);
    this.store.bump("fund");
    const r = await this.http(`${this.cfg.zcashd}/admin/send`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ to: s.ua, value: this.cfg.fundZat, ...(memo ? { memo } : {}) }),
    });
    const t = await r.text();
    if (!r.ok) throw new HouseError(503, `the house's funding wallet could not pay: ${t.slice(0, 200)}`);
    return { txid: Buffer.from(JSON.parse(t).txid).reverse().toString("hex"), value: this.cfg.fundZat };
  }

  /** Confirmations of a Zcash transaction, from the operators' own node. */
  async zcashTx(txid: string) {
    if (!/^[0-9a-f]{64}$/.test(txid)) throw new HouseError(400, "txid must be 32-byte hex");
    const node = async (method: string, params: unknown[]) =>
      (await (await this.http(this.cfg.zcashNode, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) })).json()) as any;
    const r = await node("getrawtransaction", [txid, 1]);
    if (r.error) return { txid, found: false };
    const tip = (await node("getblockcount", [])).result as number;
    const h = r.result.height as number | undefined;
    return { txid, found: true, height: h ?? null, confirmations: h ? tip - h + 1 : 0 };
  }

  private async json(url: string) {
    const r = await this.http(url);
    if (!r.ok) throw new Error(`${url}: ${r.status}`);
    return r.json() as any;
  }
}

/**
 * The player's signed transaction must have exactly the message the house built (same
 * instructions, accounts, fee payer and blockhash) and a valid signature from the player's
 * session; nothing else is accepted for co-signing.
 */
export function checkSigned(unsignedB64: string, signedB64: string, player: PublicKey): Transaction {
  let tx: Transaction;
  try { tx = Transaction.from(Buffer.from(signedB64, "base64")); } catch { throw new HouseError(400, "not a transaction"); }
  const want = Transaction.from(Buffer.from(unsignedB64, "base64"));
  if (!Buffer.from(tx.serializeMessage()).equals(Buffer.from(want.serializeMessage()))) throw new HouseError(400, "transaction differs from the one the house built");
  const mine = tx.signatures.find((s) => s.publicKey.equals(player));
  if (!mine?.signature) throw new HouseError(400, "missing your session's signature");
  // verifySignatures(false): the house's own signatures are still missing.
  if (!tx.verifySignatures(false)) throw new HouseError(400, "bad signature");
  return tx;
}
