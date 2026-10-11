#!/bin/bash
# Encrypted backup of the testnet trial's secrets and state (age, to a recipient whose
# identity is NOT on this host). Installed as /usr/local/bin/zol-backup; daily by cron.
#   /var/backups/zol/zol-<UTC timestamp>.tar.age   (last 14 kept)
# Restore (on the machine holding the identity):
#   age -d -i age-identity.txt zol-....tar.age | tar -x -C /
#   then rename each op*/gateway.db.backup to gateway.db (a consistent SQLite copy).
set -euo pipefail
RECIPIENT_FILE=/etc/zol/backup-recipient.txt
OUT=/var/backups/zol
mkdir -p "$OUT"; chmod 700 "$OUT"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
# Consistent copies of the SQLite stores (WAL mode, written while running).
for db in /home/zol/ops/op*/gateway.db; do
  mkdir -p "$work$(dirname "$db")"
  sqlite3 "$db" ".backup '$work$db.backup'"   # restore: rename to gateway.db
done
ts=$(date -u +%Y%m%dT%H%M%SZ)
tar -C / -c \
  --exclude='home/zol/ops/op*/gateway.db' --exclude='home/zol/ops/op*/gateway.db-*' \
  home/zol/ops home/zol/zakura-testnet/zakura.toml etc/caddy/Caddyfile \
  etc/systemd/system/zol-zcashd@.service etc/systemd/system/zol-gateway@.service \
  -C "$work" home \
  | age -r "$(cat $RECIPIENT_FILE)" > "$OUT/zol-$ts.tar.age"
chmod 600 "$OUT/zol-$ts.tar.age"
ls -1t "$OUT"/zol-*.tar.age | tail -n +15 | xargs -r rm -f
echo "$OUT/zol-$ts.tar.age $(stat -c %s "$OUT/zol-$ts.tar.age") bytes"
