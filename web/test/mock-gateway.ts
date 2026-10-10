// Minimal stand-in for zol-gateway's page API, for tests and `npm run mock`.
// Validates intents the way the gateway would at the HTTP boundary (512-byte canonical
// memo, tag, version, session key, signature over the app's vault address).
import { createServer, type Server } from "node:http";
import { ed25519 } from "@noble/curves/ed25519";
import { memoSigningMessage } from "../src/wire";
import { encodeOrchardUa, type ZNetwork } from "../src/ua";
import { hex, unhex } from "../src/session";

export interface MockOptions {
  app: string;
  network: ZNetwork;
  vaultAddress: Uint8Array;
  denominations?: number[];
  tip?: number;
}

const NET: Record<ZNetwork, number> = { mainnet: 0, testnet: 1, regtest: 2 };

export function startMockGateway(o: MockOptions, port = 0): Promise<{ url: string; server: Server; intents: Map<string, string>; deposits: Map<string, any[]> }> {
  const intents = new Map<string, string>(); // session hex -> memo hex
  const deposits = new Map<string, any[]>();
  const sessionUa = (session: string) => {
    // Deterministic fake diversified receiver per session.
    const r = new Uint8Array(43);
    unhex(session).forEach((b, i) => (r[i] = b ^ 0x5a));
    return encodeOrchardUa(r, o.network);
  };
  const server = createServer((req, res) => {
    res.setHeader("access-control-allow-origin", "*");
    res.setHeader("access-control-allow-headers", "content-type");
    const send = (code: number, body: unknown) => {
      res.writeHead(code, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    if (req.method === "OPTIONS") return send(204, null);
    const url = new URL(req.url!, "http://x");
    const p = url.pathname.split("/").filter(Boolean); // v1 apps {app} ...
    if (p[0] !== "v1" || p[1] !== "apps") return send(404, { error: "not found" });
    if (p[2] !== o.app) return send(404, { error: "unknown app" });
    if (req.method === "GET" && p.length === 3) {
      return send(200, {
        app: o.app, network: o.network, vault_address: hex(o.vaultAddress),
        denominations: o.denominations ?? [1_000_000, 10_000_000], tip: o.tip ?? 1_000, early_depth: 10, full_depth: 100, state: "active",
      });
    }
    if (req.method === "GET" && p[3] === "sessions" && p.length === 5) {
      return send(200, { ua: sessionUa(p[4]), receiver_index: "7", intent_registered: intents.has(p[4]) });
    }
    if (req.method === "GET" && p[3] === "sessions" && p[5] === "deposits") return send(200, deposits.get(p[4]) ?? []);
    if (req.method === "POST" && p[3] === "intents") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        try {
          const memo = unhex(JSON.parse(body).memo);
          if (memo.length !== 512 || memo[0] !== 0xff || memo[4] !== 2) return send(400, { error: "not a v2 memo" });
          const session = memo.subarray(6, 38);
          const m = {
            flags: memo[5], sessionKey: session, payoutAddress: memo.subarray(38, 81),
            expiryHeight: new DataView(memo.buffer, memo.byteOffset).getUint32(81, true), potId: memo.subarray(85, 117),
            signature: memo.subarray(117, 181), appData: memo.subarray(183, 183 + (memo[181] | (memo[182] << 8))),
          };
          if (!ed25519.verify(m.signature, memoSigningMessage(m, NET[o.network], o.vaultAddress), session)) return send(400, { error: "bad session signature" });
          const s = hex(session);
          intents.set(s, hex(memo));
          return send(200, { ua: sessionUa(s), receiver_index: "7", intent_registered: true });
        } catch (e) {
          return send(400, { error: String(e) });
        }
      });
      return;
    }
    send(404, { error: "not found" });
  });
  return new Promise((resolve) => server.listen(port, "127.0.0.1", () => {
    const a = server.address() as { port: number };
    resolve({ url: `http://127.0.0.1:${a.port}`, server, intents, deposits });
  }));
}
