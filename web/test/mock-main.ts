// `npm run mock`: the mock page API on :8787 for the dev page (APP, NETWORK, VAULT env).
import { startMockGateway } from "./mock-gateway";
import { unhex } from "../src/session";
import type { ZNetwork } from "../src/ua";

const app = process.env.APP ?? "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const network = (process.env.NETWORK as ZNetwork) ?? "regtest";
const vault = process.env.VAULT ? unhex(process.env.VAULT) : new Uint8Array(43).fill(0x33);
startMockGateway({ app, network, vaultAddress: vault }, Number(process.env.PORT ?? 8787)).then(({ url }) => console.log(`mock gateway on ${url} for app ${app} (${network})`));
