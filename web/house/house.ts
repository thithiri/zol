// The house: an automated second player for single-player coin flips (docs/plan-house-mode.md).
//
// The house holds a session key with a ledger balance (the bankroll) and a Solana fee payer.
// For a player who asks to play, it builds ONE transaction that creates a table, seats the
// house and the player with the same number of stakes (`join_rounds`) and, when it fits,
// starts it (pot and table into the rollup); the player signs it, the house adds its
// signatures and sends it. Either all of it happens or nothing does.
//
// That table is a session: the player flips in the rollup as often as both seats cover a
// stake, one stake moving per flip, with no Solana transaction in between. The house ends it
// (finish, cash out and close in one go) when a seat runs short, the player stops or wants to
// withdraw, the table sits idle, or its session deadline nears. It pays every Solana fee;
// rollup transactions are free. To the ledger and the VRF the house is an ordinary player.
import { ComputeBudgetProgram, Connection, Keypair, PublicKey, Transaction, type TransactionInstruction } from "@solana/web3.js";
import {
  allowedStakes, Coinflip, decodeApp, decodeBalance, decodeLedgerParams, decodePot, decodeReceipt, decodeTable, DLP, sendSigned, sendTx,
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
  /** Unused since sessions (a table idle for `idleSecs` is finished instead). */
  flipAfterSecs?: number;
  /** Stake per flip in zat (one of the coinflip's allowed stakes); default the smallest. */
  stake?: number;
  /** Stakes each seat buys into a session (at most; less if either balance is short). Default 10. */
  maxRounds?: number;
  /** A session with no flip for this long is finished by the house. Default 180 (the rollup's closer does it at 120). */
  idleSecs?: number;
  /** Ledger session deadline asked at start. Default 1800; it is finished 5 minutes before. */
  sessionSecs?: number;
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
  id: string; // pot id hex (a game) or a random id (a withdrawal)
  /** A coin flip, or a withdrawal of the player's balance to Zcash. */
  kind?: "game" | "withdraw";
  player: string; // session, base58
  stake: string;
  state: GameState;
  created: number;
  updated: number;
  events: GameEvent[];
  /** The unsigned create+join transaction, base64, and its blockhash expiry. */
  tx?: string;
  lastValid?: number;
  /** The create+join transaction also starts the table. */
  startsInJoin?: boolean;
  inRollupAt?: number;
  /** Seat 0 is the house, seat 1 the player. The last flip's winner. */
  winner?: number;
  playerWon?: boolean;
  /** A session: stakes each seat bought, flips so far, the player's wins, seat amounts [house, player]. */
  rounds?: number;
  flips?: number;
  wins?: number;
  amounts?: [string, string];
  lastFlipAt?: number;
  /** Ledger session deadline (ms). */
  deadline?: number;
  /** The player asked to stop (to withdraw, say). */
  stop?: boolean;
  /** The rollup's scheduled closer (`schedule_closer` signature), and attempts so far. */
  closer?: string;
  closerTries?: number;
  /** When the pot was paid out; the table may still be on its way back. */
  cashedOutAt?: number;
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
  /** The ledger's smallest withdrawal (read with the stake). */
  minExit = 0n;
  private infoCache?: { v: Awaited<ReturnType<House["computeInfo"]>>; t: number };

  /** The stake follows the ledger's parameters, which rarely change: cached 10 min. */
  async stake(): Promise<bigint> {
    if (this.stakeCache && Date.now() - this.stakeCache.t < 600_000) return this.stakeCache.v;
    const c = await this.base.getAccountInfo(this.cf.pdas.config()).catch((e) => {
      if (this.stakeCache) return null; // rate limited: keep the known stake
      throw e;
    });
    if (!c) return this.stakeCache!.v;
    const params = decodeLedgerParams(c!.data);
    this.minExit = params.minExit ?? 0n;
    const allowed = allowedStakes(params);
    const v = this.cfg.stake !== undefined ? BigInt(this.cfg.stake) : allowed.reduce((a, b) => (b < a ? b : a));
    if (!allowed.includes(v)) throw new Error(`configured stake ${v} is not one the coinflip accepts (${allowed.join(", ")})`);
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
    try {
      const v = await this.computeInfo();
      this.infoCache = { v, t: Date.now() };
      return v;
    } catch (e) {
      // A rate-limited RPC (one IP for the gateways and the house): the last good answer,
      // up to 10 minutes old, beats an error on every player's page.
      if (this.infoCache && Date.now() - this.infoCache.t < 600_000) return this.infoCache.v;
      throw e;
    }
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
      min_exit: String(this.minExit),
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
    // A session already on its way or in the rollup: flip there.
    if (live && live.kind !== "withdraw" && ["seated", "started", "in_rollup"].includes(live.state) && !live.stop) {
      return { game: live.id, resume: true, stake: live.stake, rounds: live.rounds ?? 1 };
    }
    if (live) throw new HouseError(409, live.kind === "withdraw" ? "a withdrawal is in progress" : "your last table is still closing; try again in a few seconds");
    const stake = await this.stake();
    const bal = await this.balanceOf(who);
    if (!bal || bal.amount < stake) throw new HouseError(409, `your ledger balance must be at least ${stake} zat; deposit first`);
    if (bal.exiting) throw new HouseError(409, "your balance is being paid out; deposit again to play");
    const info = await this.computeInfo();
    if (!info.can_play) throw new HouseError(503, BigInt(info.bankroll) < stake ? "the house is out of bankroll; try later" : "the house is busy; try again in a minute");

    // Both seats buy the same number of stakes: as many as both balances cover, up to maxRounds.
    const max = BigInt(this.cfg.maxRounds ?? 10);
    const houseStakes = BigInt(info.bankroll) / stake;
    const rounds = [bal.amount / stake, houseStakes, max].reduce((a, b) => (b < a ? b : a));
    const potId = crypto.getRandomValues(new Uint8Array(32));
    const seat = [
      this.cf.createTable(this.feePayer.publicKey, potId, stake, 600n, new PublicKey(this.cfg.validator)),
      this.cf.join(this.session.keypair.publicKey, potId, Number(rounds)), // seat 0: the house
      this.cf.join(who, potId, Number(rounds)), // seat 1: the player
    ];
    const bh = await this.base.getLatestBlockhash("confirmed");
    const build = (ixs: TransactionInstruction[]) => {
      const tx = new Transaction().add(...ixs);
      tx.feePayer = this.feePayer.publicKey;
      tx.recentBlockhash = bh.blockhash;
      return tx.serialize({ requireAllSignatures: false, verifySignatures: false });
    };
    // Starting in the same transaction saves a Solana round trip; it fits in 1,232 bytes
    // only just, so fall back to a separate `start` if it does not.
    let unsigned: Buffer;
    let started = true;
    try {
      unsigned = build([ComputeBudgetProgram.setComputeUnitLimit({ units: 700_000 }), ...seat, this.cf.start(this.feePayer.publicKey, potId, BigInt(this.sessionSecs()))]);
    } catch {
      unsigned = build(seat);
      started = false;
    }
    const g: Game = {
      id: hex(potId), player, stake: String(stake), state: "awaiting_signature", created: Date.now(), updated: Date.now(),
      rounds: Number(rounds), flips: 0, wins: 0, startsInJoin: started,
      events: [{ step: `table prepared: you and the house each put in ${rounds} stake${rounds === 1n ? "" : "s"} (${rounds} flip${rounds === 1n ? "" : "s"} at most)`, t: now() }],
      tx: Buffer.from(unsigned).toString("base64"), lastValid: bh.lastValidBlockHeight,
    };
    this.store.put(g);
    this.store.bump(`play:${ip}`);
    return { game: g.id, tx: g.tx, table: this.cf.pdas.table(potId).toBase58(), stake: g.stake, rounds: g.rounds, house: info.house };
  }

  /** Finish with the payout action attached; plain `finish` if the action path is refused. */
  private async finishInRollup(potId: Uint8Array) {
    const house = this.session.keypair;
    try {
      return await sendTx(this.er, [this.cf.finishAndPay(house.publicKey, potId)], house);
    } catch (e: any) {
      console.error(new Date().toISOString(), "finish_and_pay refused, plain finish:", String(e?.message ?? e).split("\n")[0]);
      return await sendTx(this.er, [this.cf.finish(house.publicKey, potId)], house);
    }
  }

  /** Keeps the Magic Action escrow (paid from the fee payer) above 0.005 SOL; checked hourly. */
  private escrowCheckedAt = 0;
  async topUpActionEscrow() {
    if (Date.now() - this.escrowCheckedAt < 3_600_000) return;
    this.escrowCheckedAt = Date.now();
    const have = await this.base.getBalance(this.cf.actionEscrow());
    if (have >= 5_000_000) return;
    const sig = await sendTx(this.base, [this.cf.topUpActionEscrow(this.feePayer.publicKey, 20_000_000n)], this.feePayer);
    console.log(new Date().toISOString(), `action escrow ${this.cf.actionEscrow().toBase58()} topped up to ${have + 20_000_000} lamports: ${sig}`);
  }

  private sessionSecs() {
    return this.cfg.sessionSecs ?? 1800;
  }

  /** The player wants to stop (to withdraw, say): the house finishes their session now. */
  stop(player: string) {
    const live = this.store.byPlayer(player).find((g) => g.kind !== "withdraw" && LIVE.includes(g.state));
    if (!live) return { stopping: false };
    if (!live.stop) {
      live.stop = true;
      live.events.push({ step: "you stopped: the house closes your table", t: now() });
      this.store.put(live);
    }
    return { stopping: true, game: live.id };
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
      g.deadline = Date.now() + this.sessionSecs() * 1000;
      if (g.startsInJoin) {
        g.state = "started";
        g.events.push({ step: "table created, you and the house seated, and sent to the rollup", t: now(), chain: "solana", sig });
      } else {
        g.state = "seated";
        g.events.push({ step: "table created; you and the house are seated", t: now(), chain: "solana", sig });
      }
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
    await this.topUpActionEscrow().catch((e) => console.error(new Date().toISOString(), "escrow", String(e?.message ?? e).split("\n")[0]));
    for (const g of this.store.active()) {
      if (this.busy.has(g.id)) continue;
      this.busy.add(g.id);
      try {
        // Run on while the game moves, so a session sets up and closes without waiting a tick per step.
        for (let i = 0; i < 6; i++) {
          const before = g.state;
          await this.step(g);
          if (g.state === before || TERMINAL.includes(g.state)) break;
          g.updated = Date.now();
          this.store.put(g);
        }
      } catch (e: any) {
        const msg = String(e?.message ?? e);
        // Rate limiting (one IP shared with the gateways) is not the game's fault: retry
        // quietly on the next tick without counting it toward giving up.
        if (/429|Too Many Requests|rate limit/i.test(msg)) continue;
        // A session in the rollup is only watched: a failed read is retried, never given up on.
        if (g.state === "in_rollup") continue;
        g.error = msg.slice(0, 400);
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
        const sig = await sendTx(this.base, [this.cf.start(this.feePayer.publicKey, potId, BigInt(this.sessionSecs()))], this.feePayer, [], 400_000);
        g.deadline = Date.now() + this.sessionSecs() * 1000;
        g.state = "started";
        g.events.push({ step: "game started: the table moves into the rollup", t: now(), chain: "solana", sig });
        return;
      }
      case "started": {
        if ((await this.base.getAccountInfo(table))?.owner.equals(DLP)) {
          g.state = "in_rollup";
          g.inRollupAt = Date.now();
          g.events.push({ step: "table in the MagicBlock rollup: flip away", t: now(), chain: "rollup" });
        }
        return;
      }
      case "in_rollup": {
        const [ti, pi] = await this.er.getMultipleAccountsInfo([table, pot]);
        if (!ti || !pi) return; // not cloned into the rollup yet
        const t = decodeTable(ti.data);
        // Closed in the rollup without us (its scheduled closer, or a player): follow it home.
        if (t.state === "Finished") {
          g.state = "finishing";
          g.events.push({ step: `table closed in the rollup after ${g.flips ?? 0} flip${g.flips === 1 ? "" : "s"}: you won ${g.wins ?? 0}; it returns to Solana`, t: now(), chain: "rollup" });
          return;
        }
        // Once per table: the rollup itself closes it when idle, short or near its deadline,
        // so it comes home even if this service is down.
        if (!g.closer && (g.closerTries ?? 0) < 3) {
          g.closerTries = (g.closerTries ?? 0) + 1;
          try {
            const sig = await sendTx(this.er, [this.cf.scheduleCloser(house.publicKey, potId, new PublicKey(this.cfg.validator))], house);
            g.closer = sig;
          } catch (e: any) {
            g.events.push({ step: "retrying: could not schedule the rollup's closer", t: now(), detail: String(e?.message ?? e).slice(0, 200) });
          }
        }
        const amounts = decodePot(pi.data).amounts;
        const stake = BigInt(g.stake);
        if (t.flips !== (g.flips ?? 0)) {
          // Wins follow from the player's seat: rounds stakes, plus one per win, minus one per loss.
          const net = Number((amounts[1] - BigInt(g.rounds ?? 1) * stake) / stake);
          g.flips = t.flips;
          g.wins = (t.flips + net) / 2;
          g.winner = t.winner;
          g.playerWon = t.winner === 1;
          g.lastFlipAt = Date.now();
        }
        g.amounts = [String(amounts[0]), String(amounts[1])];
        const flipStuck = t.state === "Flipping" && Date.now() / 1000 > Number(t.flipRequestedAt) + 65;
        const between = t.state === "Started" || t.state === "Landed" || flipStuck;
        // The rollup's own closer ends an idle table at 120 s; the house steps in only later.
        const idle = Date.now() - (g.lastFlipAt ?? g.inRollupAt ?? Date.now()) > (this.cfg.idleSecs ?? 180) * 1000;
        const late = Date.now() > (g.deadline ?? Infinity) - 300_000;
        const short = amounts[0] < stake || amounts[1] < stake;
        if (!between || !(g.stop || short || idle || late)) return;
        const sig = await this.finishInRollup(potId);
        g.state = "finishing";
        const why = g.stop ? "you stopped" : short ? (amounts[1] < stake ? "your stakes are used up" : "you took all the house's stakes") : idle ? "no flips for a while" : "the session's time is nearly up";
        g.events.push({ step: `table closed (${why}) after ${g.flips ?? 0} flip${g.flips === 1 ? "" : "s"}: you won ${g.wins ?? 0}; it returns to Solana`, t: now(), chain: "rollup", sig });
        return;
      }
      case "landed": {
        const sig = await this.finishInRollup(potId);
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
        const [ri, ti] = await this.base.getMultipleAccountsInfo([this.cf.pdas.receipt(pot), table]);
        const r = decodeReceipt(ri!.data);
        const paid = g.rounds && g.amounts ? `your ${BigInt(g.amounts[1])} zat back in your ledger balance` : g.playerWon ? "winnings credited to your ledger balance" : "pot paid to the house";
        if (r.state !== "Delegated") {
          // The table's Magic Action (cash_out_table) already paid both seats.
          g.state = "cashed_out";
          g.events.push({ step: `${paid} (paid by the rollup's action on return)`, t: now(), chain: "solana" });
          return;
        }
        // Not paid by the action (yet): pay, and close the table if it is back too.
        const ix = ti?.owner.equals(this.cf.p.app) && r.players.length === 2
          ? [this.cf.cashOutTable(potId, this.feePayer.publicKey, r.players)]
          : [this.cf.cashOut(potId, r.players)];
        const sig = await sendTx(this.base, ix, this.feePayer);
        g.state = "cashed_out";
        g.events.push({ step: paid, t: now(), chain: "solana", sig });
        return;
      }
      case "cashed_out": {
        // Return the table's rent to the house, then settle the player's balance if it auto-exited.
        // The pot's action pays as soon as the pot is back; the table returns separately, so
        // wait for it (up to a minute) rather than leave its rent behind.
        const tableOwner = (await this.base.getAccountInfo(table))?.owner;
        if (tableOwner?.equals(DLP) && Date.now() - (g.cashedOutAt ??= Date.now()) < 60_000) return;
        if (tableOwner?.equals(this.cf.p.app)) {
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
          g.events.push({ step: g.rounds ? "session over: flip again or withdraw" : g.playerWon ? "winnings added to your balance: flip again or cash out" : "round over", t: now() });
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

  // ------------------------------------------------------------ withdraw

  /**
   * Builds the withdrawal for `player`: request_exit (if not already exiting) and settle in
   * one transaction, the house as fee payer. The player signs it with the session key; the
   * ledger pays the settlement to the address bound at the first deposit, nowhere else.
   */
  async withdraw(player: string) {
    let who: PublicKey;
    try { who = new PublicKey(player); } catch { throw new HouseError(400, "session must be a base58 public key"); }
    const live = this.store.byPlayer(player).find((g) => LIVE.includes(g.state) && !(g.state === "awaiting_signature" && this.expired(g)));
    if (live && live.kind !== "withdraw") this.stop(player);
    if (live) throw new HouseError(409, live.kind === "withdraw" ? "a withdrawal is already in progress" : "closing your table first; try again in a few seconds");
    const bal = await this.balanceOf(who);
    if (!bal || bal.amount === 0n) throw new HouseError(409, "nothing to withdraw");
    const app = decodeApp((await this.base.getAccountInfo(this.cf.pdas.app()))!.data);
    const ixs = bal.exiting ? [] : [this.cf.requestExit(who)];
    ixs.push(this.cf.settle(this.feePayer.publicKey, app.nextSettlement, [who]));
    const tx = new Transaction().add(...ixs);
    tx.feePayer = this.feePayer.publicKey;
    const bh = await this.base.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash;
    const id = hex(crypto.getRandomValues(new Uint8Array(32)));
    const g: Game = {
      id, kind: "withdraw", player, stake: String(bal.amount), state: "awaiting_signature", created: Date.now(), updated: Date.now(),
      events: [{ step: `withdrawal of ${bal.amount} zat prepared`, t: now() }],
      tx: Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64"),
      lastValid: bh.lastValidBlockHeight, settlement: String(app.nextSettlement),
    };
    this.store.put(g);
    return { id, tx: g.tx, amount: String(bal.amount), settlement: g.settlement };
  }

  /** The player's signed withdrawal: checked like a join, then co-signed and sent. */
  async submitWithdraw(id: string, signedB64: string) {
    const g = this.store.get(id);
    if (!g || g.kind !== "withdraw") throw new HouseError(404, "no such withdrawal");
    if (g.state !== "awaiting_signature") throw new HouseError(409, `withdrawal is ${g.state}`);
    const tx = checkSigned(g.tx!, signedB64, new PublicKey(g.player));
    tx.partialSign(this.feePayer);
    try {
      const sig = await sendSigned(this.base, tx.serialize(), g.lastValid!);
      g.state = "settled";
      g.events.push({ step: `settlement ${g.settlement}: the operators pay ${g.stake} zat less fees to your Zcash address`, t: now(), chain: "solana", sig });
    } catch (e: any) {
      g.state = /expired/.test(String(e.message)) ? "expired" : "failed";
      g.error = String(e.message).slice(0, 400);
      g.events.push({ step: g.state === "expired" ? "took too long to sign; try again" : "the withdrawal did not go through; try again", t: now(), detail: g.error });
    }
    g.updated = Date.now();
    this.store.put(g);
    if (g.state !== "settled") throw new HouseError(409, g.error ?? g.state);
    return this.game(id);
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
