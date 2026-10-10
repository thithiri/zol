# Network setup for operator hosts

The gateway has no TLS of its own. Two kinds of traffic leave a host:

- **Peer traffic** (`/peer/*`, including `zol-gateway dkg`) goes to the other operators. Run it over WireGuard. Requests are already signed with guardian keys, but responses are not, and WireGuard authenticates both directions and hides the traffic.
- **The page API** (`/v1/*`, `/health`) is public. Put it behind a reverse proxy that terminates TLS (Caddy below), and never proxy `/peer/*`.

Everything else stays on localhost.

| Port | What | Reachable from |
|---|---|---|
| 443 | Caddy: TLS, page API | Internet |
| 51820/udp | WireGuard | The other operators' IPs |
| 8700 | `zol-gateway serve` (and `dkg`) | WireGuard (`wg0`) and localhost |
| 18300 | `zol-zcashd` | localhost only |
| 18232 / 8232 | Zakura RPC (testnet / mainnet) | localhost only |
| 18233 / 8233 | Zakura P2P (testnet / mainnet) | Internet (optional, helps the network) |

## WireGuard between three hosts

Each host makes a key pair:

```bash
wg genkey | tee wg.key | wg pubkey > wg.pub
```

Here is `/etc/wireguard/wg0.conf` on op0. Addresses are 10.77.0.1, .2 and .3. The other hosts mirror it, each listing the other two as peers.

```ini
[Interface]
Address = 10.77.0.1/24
ListenPort = 51820
PrivateKey = <op0 wg.key>

[Peer]
# op1
PublicKey = <op1 wg.pub>
Endpoint = <op1 public IP>:51820
AllowedIPs = 10.77.0.2/32
PersistentKeepalive = 25

[Peer]
# op2
PublicKey = <op2 wg.pub>
Endpoint = <op2 public IP>:51820
AllowedIPs = 10.77.0.3/32
PersistentKeepalive = 25
```

```bash
sudo systemctl enable --now wg-quick@wg0
```

## Firewall (ufw)

```bash
sudo ufw default deny incoming
sudo ufw allow 22/tcp
sudo ufw allow 443/tcp
sudo ufw allow from <op1 public IP> to any port 51820 proto udp
sudo ufw allow from <op2 public IP> to any port 51820 proto udp
sudo ufw allow in on wg0 to any port 8700 proto tcp
sudo ufw enable
```

Some providers also filter traffic in their own console, Oracle Cloud's security lists for example. Open the same ports there.

## TLS for the page API (Caddy)

`/etc/caddy/Caddyfile`:

```caddy
op0.example.com {
    @peer path /peer/*
    respond @peer 404
    reverse_proxy 127.0.0.1:8700
}
```

Caddy obtains and renews the certificate itself. Point the DNS name at the host first.

## Serve config

- `listen`: `"0.0.0.0:8700"`. The firewall limits it to `wg0` and localhost, and Caddy reaches it on localhost.
- `peers[].url`: the WireGuard address, e.g. `"http://10.77.0.2:8700"`.
- `public_url`: this host's page API, e.g. `"https://op0.example.com"`. Each `peers[].public_url` is that peer's. `AppInfo.operators` lists them for pages.
- `zcashd`: `"http://127.0.0.1:18300"`.

`zol-gateway dkg` uses the same `listen` and `peers[].url`, so it runs over WireGuard too. Run it before `serve`, since both bind the same port.

## One host for a trial

For a testnet trial on one budget VPS, the three operators can share a host. Run three `zol-zcashd` and three `zol-gateway serve` on different localhost ports, with each `peers[].url` on `127.0.0.1`. This needs no WireGuard; only Caddy faces the internet. A single Zakura node can serve all three `zol-zcashd`, although separate nodes are what make the chain-agreement halt meaningful.

This is one trust domain. Whoever controls the host holds every share, so it shows that the system runs, not that it is safe. Before real funds, each operator needs its own host, provider and keys.
