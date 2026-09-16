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
slot    = { value: hash | TOMBSTONE, ts: u64, author: pubkey }   # the LWW tiebreaker LIVES in state
state   = map<name (string), slot>                               # opaque bytes the SM owns
payload = Put   { name: string, hash: string }                   # hash is a Layer-2 SHA-256 address
        | Remove{ name: string }
```
- `initial-state()` → empty map.
- **Genesis event** (folded via `apply`) carries the per-network config: the
  **write allow-list** (a set of authorized *node* pubkeys) — not a constructor arg,
  per the mesh contract.
- **Why the slot carries `(ts, author)`** (mesh-dev correctness fix): admission-final
  hands `apply` ALL concurrent writes; it must pick the winner from their **content**,
  not fold position. A bare `map<name, hash>` has nothing to compare against, so it
  degrades to "last-in-fold-order wins" — it converges only because the fold order is
  deterministic, NOT because of the LWW intent. Storing `(ts, author)` per name makes
  `apply` a true LWW register (below): idempotent, commutative, fold-order-independent.

### 1.3 validate — write-auth + admission (PURE, ancestry-relative)
An event is `(id, author, timestamp, payload)` where `author` = the 32-byte
**verified signer pubkey** (the node that authored it — see §1.5). Admit iff:
- `author ∈ allow-list` (folded from genesis). This IS the write-auth — the
  control-plane `command_allow` shape, reused. Unauthorized writes are `Err`.
- payload is well-formed (`hash` is 64-char lowercase hex; `name` non-empty).

`validate` never needs to see other concurrent events — conflicts converge in `apply`.

### 1.4 apply — deterministic, CONFLUENT (LWW register per name)
`apply` compares the incoming event's `(ts, then author)` against the **stored** slot's
`(ts, author)` and overwrites **only if strictly greater**:
- `Put{name,hash}`: `state[name] = {hash, ts, author}` iff `(ts, author) > stored`.
- `Remove{name}`: `state[name] = {TOMBSTONE, ts, author}` iff `(ts, author) > stored`
  (a tombstone so a delete/re-add race is deterministic; `has`/reads treat TOMBSTONE as
  absent). GC can drop a tombstone once no concurrent frontier can precede it.
- Different names commute trivially; same-name concurrency resolves by the total
  `(ts, author)` order — so the fold is genuinely order-independent (a real CRDT LWW
  register), which is the confluence property the contract requires.

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

**RESOLVED (2026-09-16 — theater-dev + manager + mesh-dev): ship mesh persistence v0.**

- **(a) theater turnkey chain persistence — RULED OUT.** theater-dev, source-definitive
  (`chain/mod.rs`): events are hashed, broadcast to subscribers, and **DROPPED**; the
  runtime keeps only the rolling head hash and writes no chain file. Durability is a
  deliberate *userland* capability. It also inherits the unbounded re-fold cost — no
  scaling escape, just less code.
- **The scaling nuance (mesh-dev):** mesh is **full-retention** (v0.4 removed
  compaction), so *both* chain-replay and "persist whole node-state + re-fold" grow
  with the DAG. A *truly* bounded cold-boot needs a **folded-state snapshot** (index
  map + frontier marker) — which reintroduces map/set-format-versioning +
  checkpoint-certification discipline. So there are **three tiers**, sequenced:

  **v0 — SHIP FIRST (mesh-dev owns; `docs/DESIGN-persistence.md`):** system imports
  `theater:simple/store`, writes the **full node-state** blob on mutate,
  `node.resume(bytes)` re-folds on init. Bounded-*enough* at store scale for a long
  time (index = modest #names; re-fold is cheap until the log is genuinely large),
  **map/set-safe** (the node-state blob is list/JSON, not a packr map/set snapshot),
  and **reconcile-safe by construction** — v0 persists `self_head` + the finality
  frontier, so `resume` restores the DAG frontier and a cold-booted box re-syncs missed
  events on replug. **This already clears the acceptance bar.** ← **CHOSEN for v1 ship.**

  **v1 — later, only if re-fold cost bites:** a bounded folded-state snapshot + retained
  frontier + pruned history. This is where map/set-format-versioning +
  checkpoint-certification + frontier-retention discipline become mandatory (a snapshot
  that serves stale-local reads but can't reconcile = the trap). A deliberate partial
  reversal of full-retention — not now.

- **NOT** a store-backed *index projection* (hydrate just `map<name,hash>` from
  `theater:simple/store` — theater-dev's inbox-mailbox pattern): it drops the DAG
  frontier → can't reconcile on rejoin = the trap. Use the *node-state* snapshot.
- **Frontier confirm — ANSWERED (mesh-dev): YES.** v0 `resume` restores `self_head` +
  frontier, so reconcile-on-rejoin is guaranteed. D1 is closed in design.
- **Sequencing:** build the index SM now against the in-memory path; `resume()` is an
  additive wire-in when mesh-dev's ~4-ticket v0 build lands (mesh-dev heads-up on land).

**ACCEPTANCE BAR (manager):** the store is production-ready not at "index SM green"
but at **"cold-boot a box, serve the last-known index with the network unplugged, then
reconcile on replug."**

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
   same-name converges, `current-state` = expected map. **Build now against the
   in-memory path** (approved); D1-b (`resume()`) wires in for the prod acceptance bar.
2. **Content-replication actor** (D2): tcp fetch-by-hash + quorum replication +
   membership-seeded peers, over `content-store`.
3. **GC sweep** + durability policy.
4. **First consumer:** package/manifest distribution — publish a wasm to the store,
   boot-serve it locally (closes the http-pull boot-durability gap).

## 5. Open decisions
- **D1** — cold-boot persistence: **CLOSED → mesh persistence v0 (full node-state +
  re-fold), mesh-dev owns; ~4-ticket build.** theater turnkey ruled out; v1 bounded
  folded-snapshot deferred. Frontier/reconcile confirmed YES (v0 resume restores
  `self_head` + frontier). Acceptance bar = cold-boot serves last-known index network-
  unplugged, then reconciles on replug.
- **D2** — content-transport actor: separate tcp actor, membership-seeded. **Agreed w/ mesh-dev.**
- Digest = **SHA-256** (ratified; isolated in one fn, blake3-swappable).
- Typed `my:store.*` surface: deferred past v1 (generic `my:mesh.*` first).
- `Remove`/tombstones + quorum/replication-factor sizing: v0 values TBD (store-dev).
