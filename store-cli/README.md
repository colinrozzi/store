# store -- the deployment-distribution CLI (v1)

Makes the store the fleet's deployment-distribution mechanism (see ../DEPLOYMENT-DISTRIBUTION.md).
Central-authorized PUBLISH of a wasm into the store, and MATERIALIZE on a box: resolve a name
via the mutable index, fetch the bytes by hash (verified) to a content-addressed local path the
roster's manifest `package` field references. Boot stays local; supervisor stays store-agnostic.

Standalone binary -- the mesh client protocol is inlined (deps: ed25519-dalek, sha2,
store-protocol), so it builds + runs on any box (dev box, VPS) with no test-crate dependency.

## Commands
- `store init --index A --node-seed S` -- author Genesis: allow-list the index node's own key
  (S = that node's node_seed; it signs authored writes).
- `store publish --name N --wasm F --holder A --index A` -- hash F, PUSH bytes to a content
  holder, author `Put(N -> hash)` on the index. The central authorized step.
- `store materialize --name N --index A --holder A --root D [--out P]` -- resolve `N -> hash`
  from the index; if `<D>/packages/<hash>.wasm` exists + verifies use it, else fetch-by-hash +
  verify + write it (temp+rename). Prints the path. Idempotent; boot-local afterward.
- `store resolve --name N --index A` -- print the current `N -> hash`.

## Path convention (supervisor-dev)
`<root>/packages/<sha256>.wasm` -- content-addressed: the sha256 IS the identity (matches
digest-verify, dedups, immutable). A refresh writes a NEW digest path, so it never clobbers a
file a live actor is mmap'ing. The roster manifest's `package` field points at this path;
theater loads it unchanged (store-agnostic).

## Status -- increment 1 GREEN (local, one box)
Spawned an index node (mesh_store composite @ febee526) + a content holder (content-node);
`init` -> `publish` a 202,467-byte wasm -> `resolve` -> `materialize`: the materialized file's
sha256 == the original == the index-resolved hash, at the content-addressed path. The full
publish -> resolve -> fetch+verify -> local-file flow works as a CLI.

Next: increment 2 -- two-node (publish on the dev box, materialize on the VPS) over the
cross-machine mesh (manager deploys; mesh-dev scoping the WAN mesh).

## `store gc` -- GC-by-liveness (completes the nix-CAS)
`store gc --index <peers> --root <dir>` -- drop on-box CAS files (`<dir>/packages/<sha256>.wasm`)
whose hash is referenced by NO live index entry (the index is the GC root, nix-gc-roots style).
Safe: an immutable blob dropped here re-materializes if a future index entry needs it. So
`materialize` (populate the on-box content-addressed store) + `gc` (prune by liveness) make the
CAS the fleet's durable, boot-safe primitive -- a box boots from local `/store/<hash>` files with
NO store process running (theater reads the local path). Content = nix-CAS via materialize/gc;
index = mesh (mutable); holders = replication. (Team-settled 2026-09-17: durable CONTENT not process.)
