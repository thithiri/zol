// The house's auction desk: sealed-bid auctions (commit, then reveal; first price) where the
// house sells a numbered certificate (plan: docs/plan-sealed-auction.md, commit-reveal).
//
// The program does the rest by itself (the rollup ends the auction and the pot's return pays
// every seat); the desk opens auctions, co-signs bidders' registrations (it pays their Solana
// fee), starts each auction at its register deadline, schedules its end in the rollup, and
// steps in if the rollup does not end or pay it.
import { Buffer } from "buffer";
import { ComputeBudgetProgram, PublicKey, Transaction, type TransactionInstruction } from "@solana/web3.js";
import { Auctions, decodeAuction, decodeReceipt, DLP, sendSigned, sendTx } from "../src/chain";
import { hex } from "../src/session";
import { checkSigned, House, HouseError, TERMINAL, type Game } from "./house";

export interface AuctionTerms {
  deposit: number; // zat; also the bid cap
  registerSecs: number;
  commitSecs: number;
  revealSecs: number;
}
export const DEFAULT_TERMS: AuctionTerms = { deposit: 500_000, registerSecs: 300, commitSecs: 300, revealSecs: 300 };

const now = () => new Date().toISOString().slice(11, 19);

export class AuctionDesk {
  readonly au: Auctions;
  constructor(readonly house: House, readonly terms: AuctionTerms = DEFAULT_TERMS) {
    this.au = new Auctions(house.cf);
    house.auctions = this;
  }

  private all() {
    return this.house.store.byPlayer(this.house.session.keypair.publicKey.toBase58()).filter((g) => g.kind === "auction").sort((a, b) => b.created - a.created);
  }

  /** The auction account wherever it is: the rollup while bidding, else Solana. */
  async read(potId: Uint8Array) {
    const k = this.au.auction(potId);
    const b = await this.house.base.getAccountInfo(k);
    if (!b) return null;
    const a = b.owner.equals(DLP) ? await this.house.er.getAccountInfo(k) : b;
    return a ? decodeAuction(a.data) : null;
  }

  /** The current (or latest) auction, with its on-chain state. */
  async current() {
    const g = this.all()[0];
    if (!g) return { auction: null };
    const x = await this.read(Buffer.from(g.id, "hex")).catch(() => null);
    const { tx, ...rec } = g;
    return {
      auction: {
        ...rec,
        address: this.au.auction(Buffer.from(g.id, "hex")).toBase58(),
        chain: x && {
          state: x.state, deposit: String(x.deposit), registerDeadline: x.registerDeadline, commitDeadline: x.commitDeadline,
          revealDeadline: x.revealDeadline, seatCount: x.seatCount, players: x.players.map((p) => p.toBase58()),
          committed: x.committed, revealed: x.revealed, bids: x.bids.map(String), winner: x.winner, price: String(x.price),
          winnerKey: x.winner < x.seatCount ? x.players[x.winner].toBase58() : null,
        },
      },
    };
  }

  /** Opens an auction if none is running: the house is the seller (seat 0, a 1-zat stake). */
  async open() {
    const live = this.all().find((g) => !TERMINAL.includes(g.state));
    if (live) return this.current();
    const h = this.house;
    const potId = crypto.getRandomValues(new Uint8Array(32));
    const t = this.terms;
    const sig = await sendTx(
      h.base,
      [this.au.create(h.feePayer.publicKey, h.session.keypair.publicKey, potId, BigInt(t.deposit), [t.registerSecs, t.commitSecs, t.revealSecs], new PublicKey(h.cfg.validator))],
      h.feePayer,
      [h.session.keypair],
    );
    const at = Math.floor(Date.now() / 1000);
    const g: Game = {
      id: hex(potId), kind: "auction", player: h.session.keypair.publicKey.toBase58(), stake: String(t.deposit), state: "registering",
      created: Date.now(), updated: Date.now(), number: this.all().length + 1,
      registerDeadline: at + t.registerSecs, revealDeadline: at + t.registerSecs + t.commitSecs + t.revealSecs,
      events: [{ step: `auction opened: certificate #${this.all().length + 1}, deposit ${t.deposit} zat`, t: now(), chain: "solana", sig }],
    };
    h.store.put(g);
    return this.current();
  }

  /** A bidder's registration: the deposit into the pot, the house paying the fee. */
  async register(session: string) {
    let who: PublicKey;
    try { who = new PublicKey(session); } catch { throw new HouseError(400, "session must be a base58 public key"); }
    const g = this.all().find((x) => x.state === "registering");
    if (!g) throw new HouseError(409, "no auction is taking registrations");
    const potId = Buffer.from(g.id, "hex");
    const x = await this.read(potId);
    if (!x || x.state !== "Registering" || Date.now() / 1000 >= x.registerDeadline) throw new HouseError(409, "registration is closed");
    if (x.players.some((p) => p.equals(who))) throw new HouseError(409, "you are registered already");
    const bal = await this.house.balanceOf(who);
    if (!bal || bal.amount < x.deposit) throw new HouseError(409, `your balance must be at least the deposit, ${x.deposit} zat`);
    const tx = new Transaction().add(this.au.register(who, potId));
    tx.feePayer = this.house.feePayer.publicKey;
    const bh = await this.house.base.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash;
    const id = hex(crypto.getRandomValues(new Uint8Array(32)));
    const rec: Game = {
      id, kind: "auction-register", player: session, stake: String(x.deposit), state: "awaiting_signature", created: Date.now(), updated: Date.now(),
      events: [], tx: Buffer.from(tx.serialize({ requireAllSignatures: false, verifySignatures: false })).toString("base64"), lastValid: bh.lastValidBlockHeight,
    };
    this.house.store.put(rec);
    return { id, tx: rec.tx, auction: g.id };
  }

  async registerSigned(id: string, signedB64: string) {
    const rec = this.house.store.get(id);
    if (!rec || rec.kind !== "auction-register" || rec.state !== "awaiting_signature") throw new HouseError(404, "no such registration");
    const tx = checkSigned(rec.tx!, signedB64, new PublicKey(rec.player));
    tx.partialSign(this.house.feePayer);
    try {
      const sig = await sendSigned(this.house.base, tx.serialize(), rec.lastValid!);
      rec.state = "done";
      rec.events.push({ step: "registered: deposit in the auction's pot", t: now(), chain: "solana", sig });
    } catch (e: any) {
      rec.state = "failed";
      rec.error = String(e?.message ?? e).slice(0, 300);
    }
    this.house.store.put(rec);
    if (rec.state !== "done") throw new HouseError(409, rec.error ?? "registration failed");
    return { ok: true };
  }

  /** One step of an auction's life; called from the house's tick. */
  async step(g: Game) {
    const h = this.house;
    const potId = Buffer.from(g.id, "hex");
    const t = Date.now() / 1000;
    const validator = new PublicKey(h.cfg.validator);
    switch (g.state) {
      case "registering": {
        if (t < g.registerDeadline!) return;
        const x = await this.read(potId);
        if (!x) return;
        if (x.state === "Bidding") {
          g.state = "bidding";
          return;
        }
        if (x.seatCount >= 2) {
          const sig = await sendTx(h.base, [ComputeBudgetProgram.setComputeUnitLimit({ units: 400_000 }), this.au.start(h.feePayer.publicKey, potId)], h.feePayer);
          g.state = "bidding";
          g.events.push({ step: `bidding open: ${x.seatCount - 1} bidder${x.seatCount === 2 ? "" : "s"}; the auction is in the rollup`, t: now(), chain: "solana", sig });
          return;
        }
        // Nobody registered: the ledger refunds the seller's stake once the pot's open deadline passes.
        if (t < g.registerDeadline! + 125) return;
        const sig = await sendTx(h.base, [h.cf.refundPot(potId, x.players)], h.feePayer);
        g.state = "cancelled";
        g.events.push({ step: "nobody registered: auction cancelled", t: now(), chain: "solana", sig });
        return;
      }
      case "bidding": {
        if (!g.closer && (g.closerTries ?? 0) < 3) {
          g.closerTries = (g.closerTries ?? 0) + 1;
          if ((await h.base.getAccountInfo(this.au.auction(potId)))?.owner.equals(DLP)) {
            g.closer = await sendTx(h.er, [this.au.scheduleEnd(h.session.keypair.publicKey, potId, validator)], h.session.keypair).catch(() => undefined);
          }
        }
        if (t < g.revealDeadline! + 3) return;
        const x = await this.read(potId);
        if (x?.state === "Ended") {
          g.state = "ending";
          g.events.push({ step: "the rollup ended the auction", t: now(), chain: "rollup" });
          return;
        }
        if (t < g.revealDeadline! + 30) return; // give the rollup's own end a chance
        const sig = await sendTx(h.er, [this.au.end(potId, validator)], h.session.keypair);
        g.state = "ending";
        g.events.push({ step: "auction ended (by the house)", t: now(), chain: "rollup", sig });
        return;
      }
      case "ending": {
        const pot = h.cf.pdas.pot(potId);
        const [pi, ri] = await h.base.getMultipleAccountsInfo([pot, h.cf.pdas.receipt(pot)]);
        if (!pi?.owner.equals(h.cf.p.ledger)) return; // pot not back yet
        const r = decodeReceipt(ri!.data);
        const x = await this.read(potId);
        const result = x && x.winner < x.seatCount ? `certificate #${g.number} won for ${x.price} zat` : "no sale";
        if (r.state !== "Delegated") {
          g.state = "done";
          g.events.push({ step: `${result}; every seat paid by the rollup's action on return`, t: now(), chain: "solana" });
          return;
        }
        if (Date.now() - g.updated < 20_000) return;
        const sig = await sendTx(h.base, [this.au.cashOut(potId, r.players)], h.feePayer);
        g.state = "done";
        g.events.push({ step: `${result}; every seat paid`, t: now(), chain: "solana", sig });
        return;
      }
    }
  }
}

export type { TransactionInstruction };
