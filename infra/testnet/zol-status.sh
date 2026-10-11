#!/bin/bash
# One-screen health of the testnet trial. Installed as /usr/local/bin/zol-status.
#   zol-status          human-readable
#   zol-status --json   one JSON line (cron appends it to /var/log/zol-status.jsonl)
# Exit code 0 when healthy, 1 when any check fails (for alerting).
set -uo pipefail
JSON=0; [ "${1:-}" = "--json" ] && JSON=1
APP=$(cat /home/zol/ops/shared/app.hex 2>/dev/null)
rpc() { curl -s -m 10 -X POST -H 'content-type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" http://127.0.0.1:18232/; }
j() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }

problems=()
node_tip=$(rpc getblockcount | j 'd["result"]')
tip_time=$(rpc getblock "[\"${node_tip:-0}\",1]" | j 'd["result"]["time"]')
now=$(date +%s)
tip_age=$(( now - ${tip_time:-0} ))
[ -z "$node_tip" ] && problems+=("zakura: no RPC")
[ "$tip_age" -gt 600 ] && problems+=("zakura: tip is ${tip_age}s old")
zrestarts=$(docker inspect -f '{{.RestartCount}}' zakura-testnet 2>/dev/null || echo "?")

daemons=()
for i in 0 1 2; do
  h=$(curl -s -m 15 http://127.0.0.1:1830$i/health)
  st=$(echo "$h" | j 'd.get("scanner_tip")')
  err=$(echo "$h" | j 'd.get("last_error")')
  lag=$(( ${node_tip:-0} - ${st:-0} ))
  [ -z "$st" ] && problems+=("zcashd$i: down")
  [ -n "$st" ] && [ "$lag" -gt 5 ] && problems+=("zcashd$i: ${lag} blocks behind")
  [ -n "$err" ] && [ "$err" != "None" ] && problems+=("zcashd$i: $err")
  daemons+=("{\"op\":$i,\"scanner_tip\":${st:-null},\"lag\":$lag}")
done

gateways=()
for i in 0 1 2; do
  h=$(curl -s -m 15 http://127.0.0.1:870$i/health)
  ok=$(echo "$h" | j 'd.get("ok")')
  halted=$(echo "$h" | j 'd.get("halted")')
  [ "$ok" != "True" ] && problems+=("gateway$i: down")
  [ -n "$halted" ] && [ "$halted" != "None" ] && problems+=("gateway$i: HALTED $halted")
  gateways+=("{\"op\":$i,\"ok\":$([ "$ok" = True ] && echo true || echo false),\"halted\":$([ "$halted" = None ] || [ -z "$halted" ] && echo null || echo "\"$halted\"")}")
done
fails=$(journalctl -u 'zol-gateway@*' --since '-15min' --no-pager 2>/dev/null | grep -c 'tick:')
[ "$fails" -gt 20 ] && problems+=("gateways: $fails failed polls in 15 min")
frozen=$(journalctl -u 'zol-gateway@*' --since '-15min' --no-pager 2>/dev/null | grep -c 'guard: Froze')
[ "$frozen" -gt 0 ] && problems+=("guardian FROZE the app")

funding=$(curl -s -m 5 http://127.0.0.1:18300/admin/funding | j 'd["spendable"]')
[ -n "$funding" ] && [ "$funding" -lt 2100000 ] && problems+=("funding wallet low: $funding zat")
public=$(curl -s -m 20 -o /tmp/zol-appinfo.json -w '%{http_code}' "https://159-223-38-126.sslip.io/op0/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo")
[ "$public" != 200 ] && problems+=("public page API: HTTP $public")
# The app's state on the ledger itself, not log lines: a freeze must show until it is undone.
app_state=$(j 'd["state"]' < /tmp/zol-appinfo.json)
[ -n "$app_state" ] && [ "$app_state" != active ] && problems+=("app is ${app_state^^} on the ledger")
house=$(curl -s -m 10 http://127.0.0.1:8790/house/info)
bankroll=$(echo "$house" | j 'd["bankroll"]')
[ -n "$house" ] && [ -z "$bankroll" ] && problems+=("house: no answer")
[ -n "$bankroll" ] && [ "$bankroll" -lt 997000 ] && problems+=("house bankroll below one stake: $bankroll zat")

mem_avail=$(free -m | awk '/^Mem:/ {print $7}')
swap_used=$(free -m | awk '/^Swap:/ {print $3}')
disk_pct=$(df / | awk 'NR==2 {print $5}' | tr -d %)
[ "$mem_avail" -lt 150 ] && problems+=("memory: ${mem_avail} MB available")
[ "$swap_used" -gt 1024 ] && problems+=("swap: ${swap_used} MB used")
[ "$disk_pct" -gt 85 ] && problems+=("disk: ${disk_pct}% used")
last_backup=$(ls -1t /var/backups/zol/zol-*.tar.age 2>/dev/null | head -1)
backup_age=$(( (now - $(stat -c %Y "$last_backup" 2>/dev/null || echo 0)) / 3600 ))
[ "$backup_age" -gt 26 ] && problems+=("backup: last one ${backup_age} h old")

ok=$([ ${#problems[@]} -eq 0 ] && echo true || echo false)
if [ $JSON = 1 ]; then
  printf '{"t":"%s","ok":%s,"node_tip":%s,"tip_age_s":%s,"zakura_restarts":"%s","zcashd":[%s],"gateways":[%s],"failed_polls_15m":%s,"funding_zat":%s,"public_http":"%s","app_state":"%s","house_bankroll":"%s","mem_avail_mb":%s,"swap_mb":%s,"disk_pct":%s,"backup_age_h":%s,"problems":%s}\n' \
    "$(date -u +%FT%TZ)" "$ok" "${node_tip:-null}" "$tip_age" "$zrestarts" "$(IFS=,; echo "${daemons[*]}")" "$(IFS=,; echo "${gateways[*]}")" \
    "$fails" "${funding:-null}" "$public" "${app_state:-}" "${bankroll:-}" "$mem_avail" "$swap_used" "$disk_pct" "$backup_age" \
    "$(printf '%s\n' "${problems[@]}" | python3 -c 'import json,sys; print(json.dumps([l for l in sys.stdin.read().splitlines() if l]))')"
else
  echo "ZOL testnet  $(date -u +%T) UTC   $([ "$ok" = true ] && echo HEALTHY || echo PROBLEMS)"
  echo "  zakura     tip ${node_tip:-?} (${tip_age}s old), restarts $zrestarts"
  for d in "${daemons[@]}"; do echo "  zcashd     $d"; done
  for g in "${gateways[@]}"; do echo "  gateway    $g"; done
  echo "  polls      $fails failed in 15 min"
  echo "  funding    ${funding:-?} zat spendable"
  echo "  public     page API HTTP $public, app ${app_state:-?}"
  echo "  house      bankroll ${bankroll:-not running} zat"
  echo "  host       ${mem_avail} MB RAM free, swap ${swap_used} MB, disk ${disk_pct}%, last backup ${backup_age} h ago"
  for p in "${problems[@]}"; do echo "  !! $p"; done
fi
[ "$ok" = true ]
