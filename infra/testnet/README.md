# Testnet trial host

The live configuration of the trial's host (`159-223-38-126.sslip.io`, Ubuntu 24.04), and its
ops scripts. The runbook is [`docs/ops/testnet-deploy.md`](../../docs/ops/testnet-deploy.md).
Configs holding keys or secrets aren't here: `serve.json`, `zcashd.toml`, `house.json` and the
key files live only under `/home/zol/ops` (and in the encrypted backup).

| File | Installed as | What |
| --- | --- | --- |
| [`systemd/zol-zcashd@.service`](systemd/zol-zcashd@.service) | `/etc/systemd/system/` | One `zol-zcashd` per operator (`@0`, `@1`, `@2`) |
| [`systemd/zol-gateway@.service`](systemd/zol-gateway@.service) | `/etc/systemd/system/` | One gateway per operator; needs its daemon |
| [`systemd/zol-house.service`](systemd/zol-house.service) | `/etc/systemd/system/` | The house (single-player games) |
| [`caddy/Caddyfile`](caddy/Caddyfile) | `/etc/caddy/Caddyfile` (`root:caddy 0640`) | TLS, `/op{0,1,2}/*` to the gateways, `/peer/*` refused, `/solana` to Helius, `/house/*`, the pages |
| [`zakura/zakura.toml`](zakura/zakura.toml) | `/home/zol/zakura-testnet/zakura.toml` | The Zcash testnet node's config |
| [`cron/zol-backup`](cron/zol-backup), [`cron/zol-status`](cron/zol-status) | `/etc/cron.d/` | Daily encrypted backup; checks and alerts every 5 minutes |
| [`logrotate/zol`](logrotate/zol) | `/etc/logrotate.d/zol` | Weekly rotation of the status and backup logs |
| [`zol-status.sh`](zol-status.sh) | `/usr/local/bin/zol-status` | One-screen health check (`--json` for one line) |
| [`zol-alert.sh`](zol-alert.sh) | `/usr/local/bin/zol-alert` | Logs `zol-status` and notifies ntfy when problems change |
| [`zol-backup.sh`](zol-backup.sh) | `/usr/local/bin/zol-backup` | Encrypted (age) backup of keys, state and these configs |
| [`zol-add-app.sh`](zol-add-app.sh) | `/usr/local/bin/zol-add-app` | Adds an app to every operator ([`onboard-app.md`](../../docs/ops/onboard-app.md)) |

The units, the Zakura config, cron and logrotate files are byte-for-byte the host's (checked
2026-10-12).

## Install

```bash
install -m 644 systemd/*.service /etc/systemd/system/ && systemctl daemon-reload
systemctl enable --now zol-zcashd@{0,1,2} zol-gateway@{0,1,2} zol-house
install -m 644 cron/* /etc/cron.d/ && install -m 644 logrotate/zol /etc/logrotate.d/zol
for s in zol-status zol-alert zol-backup zol-add-app; do install -m 755 $s.sh /usr/local/bin/$s; done
```

**Zakura** runs in Docker, from its config and a cache directory (pruned state, about 9 GB):

```bash
docker run -d --name zakura-testnet --restart unless-stopped \
  -p 127.0.0.1:18232:18232 -p 18233:18233 \
  -v /home/zol/zakura-testnet/cache:/home/zebra/.cache/zakura \
  -v /home/zol/zakura-testnet/zakura.toml:/etc/zakura/zakura.toml:ro \
  zakuracore/zakura:1.6.0
```

The RPC port is published on 127.0.0.1 only; 18233 (P2P) is public.

**Caddy** reads the Helius key from its environment. The live host has the key written into
its Caddyfile instead; either works.

```bash
install -m 640 -o root -g caddy caddy/Caddyfile /etc/caddy/Caddyfile
install -m 640 -o root -g caddy /dev/null /etc/caddy/caddy.env
echo 'HELIUS_API_KEY=<key>' > /etc/caddy/caddy.env     # ZOL_HOST=<name> for another host
systemctl edit caddy     # [Service] EnvironmentFile=/etc/caddy/caddy.env
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && systemctl restart caddy
curl -o /dev/null -w '%{http_code}\n' https://<host>/op0/peer/x     # must print 404
```
