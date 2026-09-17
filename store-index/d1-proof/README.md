# d1-proof -- cold-boot persistence acceptance bar

The store's production acceptance bar (manager): "cold-boot a box, serve the last-known
index with the network unplugged, then reconcile on replug." This harness proves the
local-serve half end to end:

1. boot a 1-node store network with a **pinned** store handler (`base_path` + stable `store_id`),
2. author `Genesis` + a `Put`, confirm `current-state` has it,
3. **kill** the node (process gone),
4. **restart from the SAME data-dir** (no re-author, network unplugged -- 1-node, no peers),
5. assert `current-state` STILL has the Put.

Since there are no peers, the Put can only survive if the node **persisted + resumed** its
state -- which is mesh-dev's D1-b (`node.resume` + mesh-system store I/O, @ d6f4f529).

## Status -- D1 PASSED (2026-09-17, mesh@d6f4f529)

boot1 authored pkg/live; killed the node; restart from the same data-dir with the network
unplugged -> boot2 logged "resumed from persisted node-state" and current-state STILL had
pkg/live. Cold-boot serves the last-known index locally -- the production acceptance bar is MET.

Verified again via the standard `mesh.lib.mkComposite` off **mesh@febee526** (15 links,
resume included) -- D1 passes on the clean flake path too.

NOTE (history): on mesh@d6f4f529 the flake `mkComposite` was missing the `node.resume` link,
so the composite failed to instantiate ("unknown import: node::resume"). Fixed by mesh-dev @
febee526. `compose-manual.sh` (a manual 15-link compose) is retained only for building against
d6f4f529; on mesh >= febee526 just use `mesh.lib.mkComposite`.

## Run
Build the composite off **mesh >= febee526** (`mesh.lib.mkComposite { name="store"; sm; }`), then
`COMPOSITE=<mesh_store.wasm> cargo run --release`
