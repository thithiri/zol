#!/bin/bash
# Adds an app to every operator of the testnet trial: its id to each zol-zcashd's `apps`, and
# {program, gateway_master_secret} to each gateway's `apps`, then restarts them and prints
# the app's vault. Installed as /usr/local/bin/zol-add-app; run as root on the host.
#
#   zol-add-app <program b58>            shows what it would change (nothing is written)
#   zol-add-app <program b58> --apply    writes the configs, restarts, prints the vault
#
# Refuses unless the ledger shows the app registered and Pending (zol-devnet status), and
# unless every gateway already holds the shared master secret. Each app's gateway keys are
# derived from that one secret, so the new entry reuses it. A daemon scans a new app from
# the current tip on, so run this before the app's genesis is broadcast; an app can't be
# removed from a daemon later. The steps after it (genesis, activation, cap) are in
# docs/ops/onboard-app.md.
set -euo pipefail
PROGRAM="${1:?usage: zol-add-app <program b58> [--apply]}"
APPLY="${2:-}"
OPS=/home/zol/ops
SECRET_FILE=$OPS/shared/gateway-master-secret.hex

APP=$(python3 -c '
import sys
a = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"; n = 0
for c in sys.argv[1]: n = n * 58 + a.index(c)
print(n.to_bytes(32, "big").hex())' "$PROGRAM")
echo "program $PROGRAM"
echo "app id  $APP"

state=$(zol-devnet status --app "$PROGRAM" | tail -1 | python3 -c 'import json,sys; a=json.load(sys.stdin); print(a.get("state") or "unregistered")')
echo "ledger  $state"
[ "$state" = Pending ] || { echo "refused: the app must be registered and Pending (devnet register --app $PROGRAM)"; exit 1; }

python3 - "$OPS" "$PROGRAM" "$APP" "$SECRET_FILE" "$APPLY" <<'PY'
import json, os, re, sys, tempfile
ops, program, app, secret_file, apply = sys.argv[1:]
secret = open(secret_file).read().strip()
assert re.fullmatch(r"[0-9a-f]{64}", secret), "the master secret file must hold 64 hex chars"

def replace(path, text):
    st = os.stat(path)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path))
    with os.fdopen(fd, "w") as f:
        f.write(text)
    os.chown(tmp, st.st_uid, st.st_gid)
    os.chmod(tmp, st.st_mode & 0o777)
    os.replace(tmp, path)

# Check all three before writing any.
plans = []
for i in range(3):
    toml_path, json_path = f"{ops}/op{i}/zcashd.toml", f"{ops}/op{i}/serve.json"
    toml = open(toml_path).read()
    m = re.search(r'^apps = \[(.*)\]$', toml, re.M)
    assert m, f"{toml_path}: no apps line"
    apps = re.findall(r'"([0-9a-f]{64})"', m.group(1))
    serve = json.load(open(json_path))
    if any(e["gateway_master_secret"] != secret for e in serve["apps"]):
        sys.exit(f"refused: {json_path} holds a master secret other than {secret_file}")
    if app in apps or any(e["program"] == program for e in serve["apps"]):
        sys.exit(f"refused: op{i} already has this app")
    print(f"op{i}: zcashd apps {len(apps)} -> {len(apps) + 1}, gateway apps {len(serve['apps'])} -> {len(serve['apps']) + 1}")
    plans.append((toml_path, toml, m, apps, json_path, serve))
if apply == "--apply":
    for toml_path, toml, m, apps, json_path, serve in plans:
        line = "apps = [" + ", ".join(f'"{a}"' for a in apps + [app]) + "]"
        replace(toml_path, toml[:m.start()] + line + toml[m.end():])
        serve["apps"].append({"program": program, "gateway_master_secret": secret})
        replace(json_path, json.dumps(serve, indent=1) + "\n")
PY

if [ "$APPLY" != --apply ]; then
  echo "dry run: nothing written. Rerun with --apply."
  exit 0
fi

for i in 0 1 2; do systemctl restart "zol-zcashd@$i" "zol-gateway@$i"; done
echo "restarted the daemons and gateways; waiting for the vaults"
for _ in $(seq 1 30); do
  ok=1
  for i in 0 1 2; do curl -sf "http://127.0.0.1:1830$i/vault?app=$APP" >/dev/null || ok=0; done
  [ $ok = 1 ] && break
  sleep 2
done
vaults=$(for i in 0 1 2; do curl -sf "http://127.0.0.1:1830$i/vault?app=$APP" | python3 -c 'import json,sys; v=json.load(sys.stdin); print(v["ua"], bytes(v["address"]).hex())'; done | sort -u)
[ "$(echo "$vaults" | wc -l)" = 1 ] || { echo "the daemons disagree on the vault:"; echo "$vaults"; exit 1; }
echo "vault ua     ${vaults%% *}"
echo "vault hex43  ${vaults##* }   (for activate)"
echo "next: fund the genesis head (docs/ops/onboard-app.md, step 4)"
