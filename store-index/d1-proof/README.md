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

## Status
- On mesh@6ce46de3 (pre-D1-b): correctly reports **D1 NOT YET** -- after restart the index
  is empty (re-genesis); persistence is off. This validates the harness.
- On mesh@d6f4f529 (D1-b): expected **D1 PASSED** -- the Put survives, `resumed from
  persisted node-state` in the log. Gated on d6f4f529 reaching origin/main + bumping the
  mesh input. **No SM/driver change needed** -- just the pinned store handler (already here).

## Run
`COMPOSITE=<mesh_store.wasm built from mesh@d6f4f529> cargo run --release`
