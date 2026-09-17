# store as deployment-distribution (v1 design)

Make the store the fleet's deployment-distribution mechanism: a central authorized point
publishes a wasm (+ manifest) into the store; every box gets it, verified + durable, with
**boot staying local** (no network at read time). Tested across two real nodes (dev box +
VPS). Distribution design+build: store-dev; deployment + prod touch: the manager.

Composes what's already live-proven: the mutable index (`name→hash`), the content layer
(fetch-by-hash + replicate + verify), and cold-boot persistence (D1). The first-consumer
proof (`first-consumer-proof/`) already demonstrated *resolve-name→fetch-by-hash→verify*
end to end — this productionizes it into a CLI + a two-node flow.

## PRIORITY (Colin's call, 2026-09-17): HTTP boot-pull FIRST

**#1 — HTTP boot-pull (GREEN).** theater already fetches http `package` URLs; the boot gap was
only that the URL pointed at GitHub. A store node serves `GET /by-hash/<sha256>` -> the wasm
bytes (see `content-node/` HTTP boot-pull), so a consumer manifest sets `package =
http://<store-node>/by-hash/<hash>` and theater's existing http-pull boot-serves it from the
on-box store -- **zero new resolution code**, manifest change is one hostname. Proven locally:
published a wasm to a store node, a consumer manifest pointing at its http endpoint spawned +
ran (theater fetched `[http] 200`, `actor.init`). Caveat: theater re-fetches every spawn (no
cache), so the store node is a LIVE boot dependency -- fine for this cut.

**#2 — materialize (below).** The boot-if-store-down upgrade: write the wasm to a local pinned
path so boot survives the store being unreachable. Design + a working `store` CLI increment
already exist (`store-cli/`); it composes on top of #1.

**MODE SELECTION (supervisor-dev, agreed).** These are per-environment modes, not stages:
- **dev / light deploys → #1 http boot-pull.** Simple, one-hostname manifest change, no local
  state. Integrity rests on the store node being honest + TLS to it (theater http-pulls by
  hash but does NOT verify the returned bytes hash to `<sha256>` — verify-in-core is exactly
  what Colin ruled out of the runtime).
- **prod (the inbox) → #2 materialize.** `store materialize` fetches AND VERIFIES the SHA-256
  before writing the local file, so it is the mode that actually delivers the content-addressed
  guarantee — **verified AND network-free at boot**. This matches Colin's "core stays
  local-bytes-only" + "verify lives in the store" ruling: prod = materialize.

## The resolution point (materialize, #2) — RECOMMENDATION

**How a box turns a manifest package ref into store bytes.** Two options:

- **(A) store-CLI MATERIALIZES the wasm to the pinned local path the roster already
  references.** A `store` CLI resolves `name→hash` (index), fetches+verifies the bytes
  (content layer / local cache), and *writes them to the local file path* the manifest
  already points at. theater/supervisor then load that local file exactly as today.
- **(B) supervisor-apply resolves a store ref natively** (manifest carries a `store://`
  ref; the supervisor fetches from the store at apply time).

**RECOMMEND (A) for v1** (agrees with the manager's lean):
- **Boot stays LOCAL** — the charter's hard requirement. The wasm is a real local file;
  theater loads it with no network, store up or down.
- **Supervisor stays store-agnostic** — zero supervisor change now → small blast radius,
  fast to prod. (B) is cleaner long-term but invasive; do it later.
- **Belt-and-suspenders** — the local-pinned path still works if the store is unreachable;
  the store just *refreshes* it. Exactly the rollout the manager wants.
- **Composes the proven pieces** — materialize = first-consumer proof + a file write.

(B) is the eventual "native" endgame; parked behind (A) until the two-node replica is proven.

## Locked architecture (manager)
- **Central source of truth** — one authorized publisher writes the index + seeds content.
- **Index = ALWAYS-refetch** — the small `name→hash` pointer is re-read from the store on
  every resolution (cheap, mutable, must be current).
- **Content = LOCAL-cache** — immutable bytes fetched once by hash, cached locally forever
  (content-addressed, so a cached hash is trusted after one SHA-256 check).
- **Anchor persists / edge resyncs** — anchor nodes persist the index (D1 cold-boot); edges
  resync the index on rejoin.

## The `store` CLI (v1 surface)
A native, static binary (like the fleet's inbox CLI) run at publish + at deploy time.
- `store publish --name <name> --wasm <file> [--holder <addr>] [--index <addr>]`
  — hash the wasm, PUSH it to content holder(s) (durability), author `Put(name→hash)` on
  the index. The **central authorized** step (index write-auth = allow-listed node key).
- `store materialize --name <name> --out <path> [--index <addr>] [--cache <dir>]`
  — REFETCH the index, resolve `name→hash`; if the hash is in the local cache (verified)
  use it, else fetch-by-hash from a holder + verify + cache; **write the bytes to `<path>`**
  (the roster's pinned local path). Idempotent; boot-local afterward.
- `store resolve --name <name>` — print the current `name→hash` (debug/inspect).

The CLI reuses the proven transports: mesh-client for the index (`current-state`/`author`),
the content wire (`REQ_GET`/`BLOB`/`PUSH`, SHA-256-verified) for bytes.

## Build increments
1. **`store` CLI (publish + materialize) + a LOCAL proof** — publish a real wasm, then
   materialize it to a path on the same box; assert the materialized file == the original
   bytes (SHA-256). This is the first-consumer proof refactored into the real CLI shape.
   *(store-dev, now — no new deps; machinery already proven.)*
2. **Two-node cross-machine** — publish on the dev box (central), materialize on the VPS
   (edge) over the cross-machine mesh (mesh-dev scoping in parallel); prove the edge boots
   the materialized wasm with boot staying local-pinned.
3. **Flip boot to resolve-from-store** — only after the replica is proven durable across
   both nodes: the roster's pinned path is produced by `store materialize` at deploy time.

## Rollout (manager)
Distribute + cache on-box first; **boot stays local-pinned** until the replica is proven
durable across both nodes; THEN flip boot to resolve from the store.

## Open items (loop-in)
- **supervisor-dev:** the pinned local path convention the roster references (what path does
  `store materialize --out` target?) — confirms supervisor stays store-agnostic under (A).
- **inbox-dev:** the publish SOURCE — reuse `release.yml`'s derive (the tested in-repo
  manifests → sub-manifests) as the input to `store publish`; how to hook it in.
- **mesh-dev:** the two-node cross-machine mesh (index network spanning dev box + VPS).
- Durability RF across two nodes; anchor vs edge roles per box (from the content roster).
