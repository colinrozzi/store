# store — design (v0, captured from the 2026-09-14/15 whiteboard with Colin)

## Starting point (2026-09-15): fresh build on current theater + mesh
Colin's call: **start fresh.** A prior attempt lives in `../store-old/` (Mar–Apr 2026):
`content-store` (SHA256 CAS actor), `label`/`ntwk` (mutable labels + a **bespoke peer-sync**:
peer certs/keys, `SyncPush`), and `sync-test`/`router-sync-test`/`resilience-test`. It's on stale
foundations twice over — **pre-#204 theater** and a **peer-sync that predates the mesh** — so we're
NOT porting it. We build the new store on the **current theater runtime (post-#204)** and **replicate
via the mesh** instead of bespoke peer-sync.
**Keep store-old as REFERENCE, not code to revive:** (a) the `content-store` CAS shape is a decent
sketch of Layer 2; (b) more valuable — `sync-test`/`resilience-test` are effectively a ready-made
**test spec** for what fleet replication must survive (crashes, partitions, routing). Mine those
scenarios; write the new implementation clean.

## The core insight: two layers, opposite distribution characteristics
The store is **one store with two layers** — which is exactly theater's own store structure
(`labels/` + `data/`) lifted to fleet scale. The difference between them is **mutability**, and
that difference is the whole design.

### 1. MUTABLE INDEX — `name → content-hash`
- **Small.** Mutable ⇒ needs **consensus** ("who has the latest value of this name?").
- This is **the part that needs the mesh**: a replicated `name → hash` map as a **mesh app-SM**.
- The mesh **NEVER carries a blob** — only these small pointers.

### 2. IMMUTABLE CONTENT — `hash → bytes` (content-addressed store / CAS)
- The key **is** the hash of the value ⇒ **immutable** ⇒ **no consensus problem** (a hash means
  the same bytes on every box, forever). Trivially replicable + cacheable.
- Distributed by **fetch-by-hash**: a box missing a hash fetches it from anyone who has it.
- This layer **already exists per-box** — it's theater's store `data/<hash>`. The fleet store
  builds *on* it.

> Why this isn't premature optimization: the split isn't a *performance* trick, it's naming the two
> genuinely-different things. "How do we replicate 1.5 MB of wasm through the mesh?" is the wrong
> question — we don't; the mesh carries `name→hash` pointers, and blobs move by fetch-by-hash because
> they're immutable. Deltas / chunking / cache tiers are the real optimizations, and *those* we defer.

## Durability / replication (immutable layer)
- Replicate each content object to **at least a durability quorum** of the active network
  (≥ what the consensus network needs to make progress) so it **survives node crashes**.
- Because content is immutable, "replicate to a quorum" needs no coordination — any node with the
  bytes is authoritative; replication is just copying.
- Edge / leaf nodes **fetch-by-hash on demand + cache**; the authoritative copies live on the quorum.
- **GC / liveness:** an object is live while *some mutable index entry points at its hash*. Once
  nothing references it, it's collectable. **The mutable index is the GC root.**

## Relationship to theater's `store` handler
- theater's `store` handler = the **per-box substrate** (a local content-addressed store:
  `labels/<name>` → hash → `data/<hash>`, SHA1; we migrated exactly this v2→v3 in the cutover).
- The fleet store = the **mesh-replicated index** + the **fleet-distributed blob layer**, built on
  top of the per-box theater stores.
- **Naming (open, Colin floated it):** keep theater's `store` (a stable theater primitive many
  actors import as `theater:simple/store`) as the on-box substrate *for now*; the fleet thing is
  "the store." Revisit whether to unify/rename once the fleet store's shape is concrete — don't
  prematurely rename a stable primitive that everything imports.

## First consumer: package / manifest distribution
- Closes the http-pull **boot-durability** gap (a box pulls pinned content once, has a local
  replica, never depends on GitHub at boot).
- Exercises exactly the large-value (blob) path.
- **Reusable publish side already built:** inbox-dev's `release.yml` derive (drift-proof, derives
  sub-manifests from the tested in-repo manifests) + the secret-free manifests (acceptor reads
  secrets from the store) — these become "publish to the store" with minor changes.

## Open design questions (store-dev owns; mesh-dev consult)
1. **Mutable-index SM on the mesh:** state model + how the mesh replicates it (consensus) +
   **write-auth** — reuse the control-plane's ed25519 / `command_allow` gating?
2. **On-box persistence** of the SM's replica (theater store as the sink) so a restart reads local
   (boot-safe). Does the mesh already persist an app-SM's state per node, or is that new?
3. **Immutable blob distribution:** the fetch-by-hash transport (peer-to-peer over the mesh? a
   durable mirror?), the durability **quorum / replication factor**, and GC-by-liveness.
4. **Secrets tier (Layer 1, later):** public content needs integrity (content-addressing gives it);
   secrets need confidentiality — encryption-at-rest + access control (a real mini-vault, a higher
   bar). Same store, tiered access — or separate? Defer past the package/manifest first cut.

## Prior art to reuse
- theater's store CAS (per-box, migrated + battle-tested).
- the mesh + its app-SM model (chat, control are precedents; control plane = authed SM over mesh).
- the supervisor's authed control surface (ed25519 + TLS) as the "update from our side" pattern.
- fleet-release (GitHub releases + checksum) for the tool slice.
