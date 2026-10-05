#!/usr/bin/env bash
# (Re)connect b to the shared p2p network and ask each node to dial the other (regtest addnode).
set -euo pipefail
source "$(dirname "$0")/lib.sh"
docker network connect zol-zebra-p2p zol-zebra-b 2>/dev/null || true
ip() { docker inspect -f '{{(index .NetworkSettings.Networks "zol-zebra-p2p").IPAddress}}' "zol-zebra-$1"; }
ipa=$(ip a); ipb=$(ip b)
echo "a=$ipa b=$ipb"
rpc a addnode "[\"$ipb:18233\",\"add\"]"; echo
rpc b addnode "[\"$ipa:18233\",\"add\"]"; echo
