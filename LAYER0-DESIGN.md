# store — LAYER0 design (v0)

How the mutable `name → hash` index rides the mesh as an app-SM, and how immutable
content is distributed by fetch-by-hash. Grounded in the mesh `node.pact` /
`state-machine.pact` contracts and mesh-dev's answers (2026-09-16). Companion to
`DESIGN.md` (the two-layer model) and `content-store/` (Layer 2, already GREEN).

> Status of inputs: the index-as-SM shape, write-auth, and compose story are
> **inheritable today**. Two things are pinned as decisions below: **(D1) cold-boot
> persistence** (not inherited today) and **(D2) the content-transport actor**.

---

## 0. Recap — two layers, opposite distribution characteristics

| | Layer 1 — mutable index | Layer 2 — immutable content |
|--|--|--|
| maps | `name → content-hash` | `sha256 → bytes` |
| mutable? | yes → needs consensus | no (key *is* the hash) |
| lives on | the **mesh**, as an app-SM | per-box CAS + fetch-by-hash |
| carries blobs? | **never** — pointers only | yes |
| status | this doc | **built + GREEN** (`content-store/`) |

The mesh replicates only the small `name → hash` pointers. Blobs move separately,
by hash, because they are immutable. The index is the **GC root**: content is live
while some index entry points at its hash.

---

## 1. Layer 1 — the mutable index as a mesh app-SM

### 1.1 It is just an SM, composed like `bank`
The index EXPORTS `state-machine.{initial-state, validate, apply, members}` (PURE,
CONFLUENT, STRUCTURE-BLIND) and is composed into the node via
`mesh.lib.mkComposite { name = "store"; sm = store_sm.wasm }` — the same path as
`bank-sm` / `chat-sm`. Built with `packr-guest 0.15` (same lineage as `content-store`).

### 1.2 State + events
```
state   = map<name (string), hash (sha256 hex string)>       # opaque bytes the SM owns
payload = Put   { name: string, hash: string }               # hash is a Layer-2 SHA-256 address
        | Remove{ name: string }                             # (tombstone; optional in v0)
```
- `initial-state()` → empty map.
- **Genesis event** (folded via `apply`) carries the per-network config: the
  **write allow-list** (a set of authorized *node* pubkeys) — not a constructor arg,
  per the mesh contract.

### 1.3 validate — write-auth + admission (PURE, ancestry-relative)
An event is `(id, author, timestamp, payload)` where `author` = the 32-byte
**verified signer pubkey** (the node that authored it — see §1.5). Admit iff:
- `author ∈ allow-list` (folded from genesis). This IS the write-auth — the
  control-plane `command_allow` shape, reused. Unauthorized writes are `Err`.
- payload is well-formed (`hash` is 64-char lowercase hex; `name` non-empty).

`validate` never needs to see other concurrent events — conflicts converge in `apply`.

### 1.4 apply — deterministic, CONFLUENT (LWW per name)
- `Put{name,hash}`: set `state[name] = hash`. Concurrent Puts to **different** names
  commute trivially. Concurrent Puts to the **same** name are resolved
  **last-writer-wins by (timestamp, then author-pubkey) tiebreak** — a total, pure
  order on the two events, so every node converges regardless of fold order.
- `Remove{name}`: delete (LWW vs Puts by the same tiebreak).

`members(state)` → the allow-list pubkeys (feeds the mesh's witness-based finality;
dormant in admission-final v0 but declared for interface-hash stability).

### 1.5 Write path — the NODE's key is the authority
- A write is `author(payload)` on a local node; **the node signs with ITS own key**
  (`node_seed → pubkey`). So the allow-listed identity is the **node** pubkey, not the
  caller's. The allow-list folds node-pubkeys.
- "Writable from a central authorized point" = one (or a few) **allow-listed node(s)**.
  An off-mesh publisher injects a `Put` by reaching an allow-listed node's actor and
  calling `author` — via `theater:simple/rpc.call` to the node's `my:mesh.author`
  (how mesh-client `Session::author` / the drivers author today), or the
  message-server path. It does **not** host the node, but needs a channel to it;
  cross-machine needs a bridge (the chat-web→driver pattern). Publishers authenticate
  out-of-band to that node; the node's allow-listed key signs.

### 1.6 Read path + interface surface
- Reads are pure queries: `current-state` folds the SM at the frontier → the
  `name → hash` map; `event-status` for a specific write.
- **v1 interface:** compose the **generic mesh-system** entry and drive it over its
  `my:mesh.*` RPC (`author` = Put, `current-state` = the folded index, `members`).
  Config JSON: `{ node_seed, listen_addr?, dial?[{pubkey,address}], tick_ms? }`.
- **Later (typed surface):** a small typed store-system entry (`my:store.put` /
  `my:store.resolve`) following the `counter-system` `StateCell`-in-cdylib pattern.
  Deferred — the generic entry is enough to prove Layer 1.

### 1.7 Network identity
The store runs as its **own named mesh network** — own genesis, own allow-list, own
node identities. It does **not** share the control mesh (each network is an
independent DAG/genesis).

---

## 2. Persistence / boot-safety — **DECISION D1 (the one real gate)**

The charter requires **boot-safe local reads: no network at read time**. What we
inherit vs. what we must add:

- **Warm restart (theater up, actor restarts): COVERED.** node-state is in-module
  (`#[derive(State)]`); theater replays the recorded chain (init + recorded host-call
  results incl. gossip bytes) and re-folds with no live network. Local reads work.
- **Cold boot (host/process gone): NOT inherited today.** The recorded chain is not
  persisted to disk turnkey (theater's replay is an explicit drive→record→replay
  path; `save_chain` is not wired). So after a cold boot the node has no local state
  until it re-syncs from peers — which **violates** "no network at read time."

**Options (pick one before building the index):**
- **(D1-a) theater persists the actor chain across cold boot** + auto-replays on
  init → store inherits cold-boot durability for free. *Needs a theater-dev confirm
  (does this exist / is it planned?).*
- **(D1-b) mesh node-state persistence** (mesh-dev's `docs/DESIGN-persistence.md`,
  spec'd, not landed): the system imports `theater:simple/store`, writes the node
  blob on mutate, `node.resume(bytes)` on init. mesh-dev has offered to own this.
  Store consumes it; persists the *full* DAG state (minimal re-sync on boot).

**Recommendation:** **co-time with (D1-b)** — it is concrete, mesh-dev owns it, and it
persists the whole node-state (not just the index projection), so a cold-booted box
serves the last-known index locally and reconciles in the background. Pursue a
theater-dev confirm on (D1-a) in parallel as a possibly-simpler alternative. **Until
one lands, the store is warm-restart-safe but not cold-boot-safe — flag before prod.**

---

## 3. Layer 2 — immutable content (built) + fetch-by-hash — **DECISION D2**

### 3.1 Per-box CAS — DONE
`content-store/` is GREEN: SHA-256-addressed `put/get/has` over `theater:simple/store`
(used as an opaque byte sink; the store owns its SHA-256, not theater's SHA-1), with
`get` digest-verifying. This is the local seat every box holds.

### 3.2 Fetch-by-hash transport — a SEPARATE content actor (NOT the node's transport)
Per mesh-dev: the node's tcp links + gossip are dedicated to DAG replication (small
events) and its peer table is internal; **do not push blobs through it.** Instead:
- A **content-replication actor** over the **theater tcp handler** with its **own**
  connections, hash-addressed fetch (`request(hash) → bytes`, verified against the
  hash on receipt — the same SHA-256 check `content-store` already does).
- **Peer set** seeded from mesh **membership** (`Session::members()` → the network's
  member pubkeys) and/or the allow-list; resolve `pubkey → address` from a registry.
- **Durability quorum:** replicate each blob to ≥ what the consensus network needs to
  make progress, so it survives node crashes. Because content is immutable, "replicate
  to a quorum" needs no coordination — any node with the bytes is authoritative.
- **Edges / leaves:** fetch-on-miss + cache; authoritative copies live on the quorum.

So: the **mesh** gives the index (`name → hash`) + membership (who the peers are); the
**content actor** does the blob fetch over its own transport. **D2 = build this actor**
(after the index SM; it depends on Layer 1 for the hash list + membership).

### 3.3 GC by liveness (store-dev owns)
An object is live while some index entry points at its hash. The **index is the GC
root**: when nothing references a hash, it is collectable. GC is a per-box sweep over
`content-store` keyed on the current index — no consensus needed (deleting a
locally-unreferenced immutable blob is safe; it can always be re-fetched if a new
index entry reintroduces the hash).

---

## 4. Build order

1. **Index SM** (`store-sm`): state + Put/Remove + validate(allow-list) + apply(LWW) +
   members. Compose via `mkComposite` with the generic mesh-system; drive over
   `my:mesh.*`. Prove: authorized Put admitted, unauthorized rejected, concurrent
   same-name converges, `current-state` = expected map. **Blocked on D1** (don't ship
   without a cold-boot answer).
2. **Content-replication actor** (D2): tcp fetch-by-hash + quorum replication +
   membership-seeded peers, over `content-store`.
3. **GC sweep** + durability policy.
4. **First consumer:** package/manifest distribution — publish a wasm to the store,
   boot-serve it locally (closes the http-pull boot-durability gap).

## 5. Open decisions
- **D1** — cold-boot persistence: (a) theater chain persistence [confirm w/ theater-dev]
  vs (b) mesh node-state persistence [mesh-dev owns, spec'd]. **Recommend (b), co-timed.**
- **D2** — content-transport actor: separate tcp actor, membership-seeded. **Agreed w/ mesh-dev.**
- Digest = **SHA-256** (ratified; isolated in one fn, blake3-swappable).
- Typed `my:store.*` surface: deferred past v1 (generic `my:mesh.*` first).
- `Remove`/tombstones + quorum/replication-factor sizing: v0 values TBD (store-dev).
