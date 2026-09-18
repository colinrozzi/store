#!/bin/sh
# inbox-dev deploy publish step (piece 1): author each inbox actor's freshly-built wasm into the store.
# Run from inbox-dev's container after building the wasms. Reads an actor->wasm map and, for each:
#   store publish  -> hash the wasm, PUSH bytes to the holders (RF), author Put(name -> hash) on the index.
# The re-authored label gossips to the cluster; each consuming box then `store materialize`s it, which
# repoints the local /var/lib/store/by-name/<name>.wasm symlink to the new content (see RUN/README).
#
# Usage: ./publish.sh <actors.json>
#   actors.json = [{"name":"inbox_acceptor","wasm":"/path/to/acceptor.wasm"}, ...]
#   (inbox-dev's ops/inbox-actors.json feeds this; names are the manifest labels.)
#
# Env (fill for your cluster; comma lists = failover / replicate-to-all):
#   INDEX  -- index endpoints, e.g. <WRITER_WG_IP>:9700,<PEER1_WG_IP>:9700,...
#   HOLDER -- holder endpoints, e.g. <WRITER_WG_IP>:9710,<PEER1_WG_IP>:9710,...  (publish PUSHes to ALL = RF)
set -eu
STORE="$(dirname "$0")/store"
ACTORS="${1:?usage: publish.sh <actors.json>}"
: "${INDEX:?set INDEX=host:9700[,host:9700...]}"
: "${HOLDER:?set HOLDER=host:9710[,host:9710...]}"

# minimal JSON array walk (no jq dependency assumed on the box; falls back to jq if present)
if command -v jq >/dev/null 2>&1; then
  jq -c '.[]' "$ACTORS" | while read -r row; do
    name=$(printf '%s' "$row" | jq -r '.name')
    wasm=$(printf '%s' "$row" | jq -r '.wasm')
    echo ">> publish $name <- $wasm"
    "$STORE" publish --name "$name" --wasm "$wasm" --index "$INDEX" --holder "$HOLDER"
  done
else
  echo "jq not found -- install jq, or invoke '$STORE publish --name N --wasm F --index $INDEX --holder $HOLDER' per actor" >&2
  exit 1
fi
echo "published all actors. Consuming boxes: 'store materialize --name <N> --root /var/lib/store --index $INDEX --holder $HOLDER' then 'supervisor restart <N>'."
