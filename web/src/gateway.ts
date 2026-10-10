// Client for the gateway's public page API (crates/zol-types/src/lib.rs `pub mod page`).
//   GET  /v1/apps/{app}                              -> AppInfo
//   GET  /v1/apps/{app}/sessions/{session}           -> SessionInfo
//   POST /v1/apps/{app}/intents  {memo}              -> SessionInfo
//   GET  /v1/apps/{app}/sessions/{session}/deposits  -> DepositStatus[]
// {app}: program ID base58; {session}: 32-byte session key, hex.

export interface AppInfo {
  app: string;
  network: "regtest" | "testnet" | "mainnet";
  vault_address: string; // 43 bytes hex
  denominations: number[]; // zatoshi
  tip: number;
  early_depth: number;
  full_depth: number;
  /** Every operator's page API base URL (intents are gossiped between them). */
  operators?: string[];
  state: "pending" | "activating" | "active" | "frozen" | "unregistered";
}
export interface SessionInfo {
  ua: string;
  receiver_index: string; // u128 as a decimal string
  intent_registered: boolean;
}
export interface DepositStatus {
  cmx: string;
  value: number;
  height: number;
  confirmations: number;
  state: "seen" | "held" | "credited" | "voided" | "refunded";
}

export class GatewayError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

export class Gateway {
  constructor(readonly base: string, private readonly fetchImpl: typeof fetch = (...a) => fetch(...a)) {}

  private async req<T>(path: string, init?: RequestInit): Promise<T> {
    const r = await this.fetchImpl(this.base.replace(/\/$/, "") + path, init);
    const text = await r.text();
    let body: any;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = { error: text };
    }
    if (!r.ok) throw new GatewayError(r.status, body?.error ?? `HTTP ${r.status}`);
    return body as T;
  }

  app(app: string) {
    return this.req<AppInfo>(`/v1/apps/${app}`);
  }
  session(app: string, sessionHex: string) {
    return this.req<SessionInfo>(`/v1/apps/${app}/sessions/${sessionHex}`);
  }
  /** Registers the signed memo (canonical 512 bytes, hex) before the player pays. */
  registerIntent(app: string, memoHex: string) {
    return this.req<SessionInfo>(`/v1/apps/${app}/intents`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ memo: memoHex }),
    });
  }
  deposits(app: string, sessionHex: string) {
    return this.req<DepositStatus[]>(`/v1/apps/${app}/sessions/${sessionHex}/deposits`);
  }
}
