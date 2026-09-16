# store — content transport (D2) design (v0)

Fetch-by-hash for the immutable content layer: how a box that is **missing** a hash gets
the bytes from a peer that has them, verified, and how content reaches a **durability
quorum** so it survives crashes. This is what makes Layer 2 *fleet-distributed* rather
than per-box. Companion to `LAYER0-DESIGN.md` §3 and `content-store/` (the per-box CAS,
already GREEN). Agreed in shape with mesh-dev + manager (2026-09-16).

> The mesh (Layer 1) carries the `name → hash` index + membership; it **never carries a
> blob**. The content transport is a **separate actor over its own tcp**, NOT the node's
> DAG-gossip transport (mesh-dev was explicit — don't overload the node).

---

## 1. The actor: `content-node`

One per box, alongside the local `content-store` CAS. It owns:
- a **tcp listener** (serve blob requests from peers) + **outbound dials** (fetch from peers),
- the local `content-store` (the byte sink; `put`/`get`/`has`, SHA-256, digest-verified),
- a **peer table** (`pubkey → address`) and the **membership** view.

It is deliberately dumb: `hash → bytes`, immutable, no consensus. All the hard mutable
stuff lives in Layer 1; this layer just moves immutable bytes to where they are needed.

### Interface (message-server / rpc verbs)
- `has(hash) -> bool` — local check.
- `get(hash) -> bytes` — local; if missing, **fetch-on-miss** from a peer (below), then cache + return; error if no peer has it.
- `put(bytes) -> hash` — store locally + **replicate** to a durability quorum (below).
- `fetch(hash) -> ok` — force a fetch-on-miss now (pre-warm).

---

## 2. Wire protocol (peer ↔ peer, over tcp)

Self-framed, tiny, request/response. One connection carries many requests.
```
REQ_HAVE  hash            -> HAVE hash | MISS hash        # cheap existence probe
REQ_GET   hash            -> BLOB hash len bytes | MISS hash
PUSH      hash len bytes  -> ACK hash                     # replication (sender pushes to a quorum peer)
```
Framing: `[op:u8][hash:32][len:u32 BE?][bytes…]`. Every `BLOB`/`PUSH` payload is
**verified against `hash` on receipt** (recompute SHA-256; drop on mismatch) — the same
check `content-store::get` already does. Content is immutable, so there is no version,
no ordering, no auth-on-read needed: a hash means the same bytes forever, from anyone.

> Integrity, not confidentiality: public content needs integrity (content-addressing +
> SHA-256 gives it). Secrets (Layer 1 tier, later) need encryption-at-rest + access
> control — out of scope for D2.

---

## 3. Peer discovery — from the mesh, not reinvented

- **Membership** is the mesh's, via `Session::members()` → the network's member **pubkeys**
  (the "use what the node computes" rule). The content-node reads members from the local
  store index network (or is handed them).
- **`pubkey → address`** resolves from a registry (same source the node's `dial` list uses;
  for v0, a static config / the roster). Seed the peer set from `members() ∪ allow_list`.
- No separate membership protocol — the store index network already knows who the peers are.

---

## 4. Durability quorum (replication)

- On `put`, replicate the blob (via `PUSH`) to **≥ a durability quorum** of peers — at least
  what the consensus network needs to make progress — so the object **survives node crashes**.
- Because content is **immutable**, "replicate to a quorum" needs **no coordination**: any
  node holding the bytes is authoritative; replication is just copying. No leader, no order.
- **Authoritative copies** live on the quorum; **edges/leaves** hold nothing until they
  `get` (fetch-on-miss) and then **cache**.
- **Quorum sizing (v0):** `RF = min(peers, 3)` or `floor(N/2)+1` of the active network,
  whichever is smaller — pick a concrete v0 value with mesh-dev when wiring; the policy knob
  lives here, not in the mesh.
- **Fetch-on-miss order:** probe `REQ_HAVE` a few peers (prefer quorum holders), `REQ_GET`
  from the first `HAVE`. Cache the result. Retry/next-peer on failure.

---

## 5. GC by liveness (index is the GC root)

- An object is **live** while some **index entry** (Layer 1 `name → hash`) points at its hash.
- GC is a **per-box sweep**: read the current index (`current-state` of the store network),
  build the live-hash set, and drop local `content-store` objects not in it. No consensus —
  deleting a locally-unreferenced *immutable* blob is always safe (it can be re-fetched if a
  future index entry reintroduces the hash).
- **Care:** only GC a hash that is unreferenced *and* below the durability quorum would still
  hold enough copies — i.e. an edge cache can GC freely; a quorum holder must not drop a hash
  that is still index-live. v0: quorum holders GC only hashes absent from the index; edges GC
  by LRU/TTL. Tombstones in the index (from `Remove`) drop the last reference → collectable.

---

## 6. Build order (v0 → v1)

1. **v0a — fetch-by-hash between two boxes.** Two `content-node`s over tcp: A `put`s bytes;
   B `get`s the hash it lacks → `REQ_GET` → A serves → B verifies SHA-256 + caches. Prove:
   B ends up with the identical, verified bytes. (Mirrors the CAS + SM proof style: a small
   driven scenario, asserted.) This is the core primitive; everything else composes on it.
2. **v0b — replicate-on-put to a quorum** (`PUSH` to RF peers) + fetch-on-miss peer probing.
3. **v1 — membership-driven peer set** (`Session::members()`), registry resolution, GC sweep
   keyed on the live index, quorum-aware GC.

## 7. Open items
- Quorum RF concrete value + how "active network size" is read (from the store index network's
  members) — settle with mesh-dev when wiring.
- Transport auth: v0 is membership-permissive (like the mesh handshake) — content is public +
  integrity-checked, so an unauthorized reader gaining public bytes is not a breach. Revisit
  for the secrets tier.
- Chunking / deltas / cache tiers: deferred (real optimizations, not v0 — per DESIGN.md).
- Does `content-node` share a box with the store index node, or run standalone reading the
  index over rpc? v0: standalone actor that queries the local store network's `current-state`
  for the live set + `members()` for peers.
