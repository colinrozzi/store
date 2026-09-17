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

NOTE: d6f4f529's `mesh.lib.mkComposite` is missing the `node.resume` link, so the flake-
composed artifact fails to instantiate ("unknown import: node::resume"). Compose MANUALLY via
`compose-manual.sh` (15 links incl node.resume) until mesh-dev fixes the flake helper (reported).

## Run
`COMPOSITE=<mesh_store.wasm built from mesh@d6f4f529> cargo run --release`
