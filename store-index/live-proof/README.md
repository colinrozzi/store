# live-proof — the store index SM, driven end-to-end on a real mesh node

Beyond the host-fold unit tests (`../store-sm`), this authors real events through a
**composed, running** `mesh_store` node and asserts `current-state` — proving the SM's
validate/apply survive the full compose → spawn → author → fold → read path.

## What it proves (all green)
- a **pre-genesis Put is rejected** by `validate` live inside the node
  (`rejected: store not initialized (no genesis)`),
- a **Genesis** seeds the allow-list with the node's pubkey,
- two **authorized Puts** (author = the node's own key, now allow-listed) are admitted,
- `current-state` decodes to the expected `IndexState` (genesis_done, allow_list=[node_pk],
  the two names resolve to their hashes, an absent name resolves to None).

## Recipe (what was run, 2026-09-16)
```sh
# 1. build the SM + compose with the mesh (packr 0.24 realized in-sandbox)
nix build --impure --expr 'let m=builtins.getFlake "github:colinrozzi/mesh"; lib=m.lib.x86_64-linux;
  sm=lib.buildWasm{pname="store-sm";src=/repo/store-index;crate="store-sm";wasmName="store_sm.wasm";};
  in lib.mkComposite{name="store";sm="${sm}/store_sm.wasm";}' -o mesh_store
# -> "Composed 3 component(s), 14 link(s) -> mesh_store.wasm"

# 2. theater matching the mesh's pin (github theater@00b0bf93), on THEATER_BIN's path
nix build --impure "github:colinrozzi/theater?rev=00b0bf93fe69a231463d3ba918fa435c5f2a517d#default" -o theater

# 3. build + run the driver (mesh-testkit from the mesh checkout, store-protocol from ../)
COMPOSITE=$(readlink -f mesh_store/mesh_store.wasm) cargo run --release
```

Node manifest handlers (theater 00b0bf93): `self` / `tcp` / `timer` / `message-server`
/ `store`; 1-node config `{"node_seed":"…","listen_addr":"127.0.0.1:PORT"}`. A 1-node
network is admission-final, so an authored event is final immediately — `submit()` then
`current_state()` reflects it with no `tick()` drive needed.
