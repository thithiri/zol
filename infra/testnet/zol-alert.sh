#!/bin/bash
# Phone alerts for the testnet trial through ntfy.sh. Installed as /usr/local/bin/zol-alert,
# run by cron every 5 minutes (it also appends the status line to /var/log/zol-status.jsonl).
#   Topic: /etc/zol/ntfy-topic (a long random name: anyone who knows it can read the alerts).
#   Subscribe in the ntfy app (iOS/Android) or at https://ntfy.sh/<topic>.
# Sends only when the set of problems changes: a new problem, a different one, or recovery.
# The messages carry problem summaries only: no keys, addresses or balances beyond the
# funding and bankroll warnings zol-status already prints.
#   zol-alert --test   send a test notification
set -uo pipefail
TOPIC=$(cat /etc/zol/ntfy-topic)
STATE=/var/lib/zol/alert-problems
mkdir -p /var/lib/zol

send() { # title priority tags message
  curl -s -m 20 -H "Title: $1" -H "Priority: $2" -H "Tags: $3" -d "$4" "https://ntfy.sh/$TOPIC" >/dev/null
}

if [ "${1:-}" = "--test" ]; then
  send "ZOL testnet: test alert" default "white_check_mark" "Alerts work. $(zol-status | head -1)"
  exit 0
fi

line=$(zol-status --json)
echo "$line" >> /var/log/zol-status.jsonl
seen=$(echo "$line" | python3 -c 'import json,sys; print("\n".join(sorted(json.load(sys.stdin)["problems"])))')
# A service busy proving a Zcash transaction, or restarting, can miss one health check.
# Alert on a problem only once it shows in two checks in a row (5 min apart); a freeze or
# a halt alerts at once.
last=$(cat /var/lib/zol/last-seen 2>/dev/null || true)
printf '%s' "$seen" > /var/lib/zol/last-seen
now=$(comm -12 <(echo "$seen" | sort) <(echo "$last" | sort) | sed '/^$/d')
urgent=$(echo "$seen" | grep -iE "FROZEN|HALTED" || true)
now=$(printf '%s\n%s\n' "$now" "$urgent" | sed '/^$/d' | sort -u)
before=$(cat "$STATE" 2>/dev/null || true)
[ "$now" = "$before" ] && exit 0
printf '%s' "$now" > "$STATE"
if [ -n "$now" ]; then
  # A freeze or a halt is urgent; the rest (low funds, lag) is high.
  prio=high
  echo "$now" | grep -qiE "FROZEN|HALTED|down" && prio=urgent
  send "ZOL testnet: PROBLEMS" "$prio" "warning" "$now
Check: ssh root@159.223.38.126 zol-status"
else
  send "ZOL testnet: healthy again" default "white_check_mark" "All checks pass. Was:
$before"
fi
