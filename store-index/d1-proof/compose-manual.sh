#!/usr/bin/env bash
# Manual store composite build for D1 -- WORKAROUND for the d6f4f529 flake bug:
# mesh.lib.mkComposite's composeManifest is missing the `node.resume` link (mesh-system
# imports node.resume but only 14 links are wired), so the flake-composed artifact fails
# to instantiate ("unknown import: node::resume"). Until mesh-dev adds the link to the
# flake, compose manually with 15 links (the 14 + node.resume). Reported to mesh-dev.
set -euo pipefail
REV=d6f4f52932feca5338e931c4bcb59e8874d414d0
M="github:colinrozzi/mesh?rev=$REV"
NODE=$(readlink -f "$(nix build --impure --no-link --print-out-paths "$M#node")"/*.wasm)
SYS=$(readlink -f "$(nix build --impure --no-link --print-out-paths "$M#mesh-system")"/*.wasm)
SM=$(readlink -f "$(nix build --impure --no-link --print-out-paths --expr \
  "let m=builtins.getFlake \"$M\"; in m.lib.x86_64-linux.buildWasm{pname=\"store-sm\";src=./..;crate=\"store-sm\";wasmName=\"store_sm.wasm\";}")"/store_sm.wasm)
PACKR=$(nix build --no-link --print-out-paths "github:colinrozzi/pack/release-v0.24.0#packr")/bin/packr
man=$(mktemp)
{ printf '[[component]]\nname="mesh-system"\nwasm="%s"\nentry=true\n[[component]]\nname="mesh"\nwasm="%s"\n[[component]]\nname="store-sm"\nwasm="%s"\n' "$SYS" "$NODE" "$SM"
  for fn in init on-connect on-bytes on-close tick author subscribe current-state current-members event-status resume; do
    printf '[[link]]\nconsumer="mesh-system"\nimport="node.%s"\nprovider="mesh"\nexport="%s"\n' "$fn" "$fn"; done
  for fn in initial-state validate apply members; do
    printf '[[link]]\nconsumer="mesh"\nimport="state-machine.%s"\nprovider="store-sm"\nexport="%s"\n' "$fn" "$fn"; done
} > "$man"
"$PACKR" compose "$man" --output "${1:-mesh_store.wasm}"
echo "composed -> ${1:-mesh_store.wasm} (15 links, node.resume included)"
