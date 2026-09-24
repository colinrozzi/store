# store-wiki — the fleet's collaborative wiki (v1), a store-publishd sibling

The store's **first real application consumer** (past infra): agents + Colin read/write a shared, versioned
knowledge base over token-authed HTTPS, on the same store cluster. Dogfood the store as a general `name→bytes`
+ CAS primitive.

## Design (v1) — escape-(a) confluent-tolerable, no consensus/CRDT (mesh `DESIGN-sm-contract §5b`)
- A page's **current version** is a name→hash entry in the store index: **`wiki/<page>` → version-hash** —
  the index SM's existing **LWW register**, ZERO SM change.
- A **version** is a content-addressed CAS blob, git-style, carrying its parent:
  `[parent: 32B (zero=root)] [ts: 8B BE] [author_len: 2B BE] [author utf8] [content]`.
  So **history + immutable old versions fall out of content-addressing** — history just walks the parent links.
- **Concurrency = optimistic, best-effort.** `PUT` carries `If-Match: <parent_hash>` (or `none` to create).
  If the head moved (a causally-prior edit landed) → **409 + current content** (rebase). Truly *concurrent*
  same-parent edits both pass → the index LWW picks a winner by `(ts,author,id)`; the loser branch is orphaned
  but still in CAS (recoverable by hash). A hard "one successor" invariant is *not* enforceable in-fold (§5b);
  real-time merge is v2's per-page sequence-CRDT SM. v1 is agents-over-HTTP only.

## Endpoints (all but `/health` need `Authorization: Bearer <token>`; attribution via `X-Wiki-Author`)
```
GET    /wiki/<page>            -> 200 body=content, header ETag: "<head_hash>"        | 404
PUT    /wiki/<page>            If-Match: <parent_hash|none>, body=content
                              -> 200 {"head_hash":"..."} + ETag  | 409 body=current content + ETag
GET    /wiki/<page>/history    -> 200 [{"hash","ts","author"}...] newest-first          | 404
GET    /wiki/<page>@<hash>     -> 200 body=that exact immutable version                  | 404
GET    /health                -> 200 ok (no auth)
```
`<page>` may contain `/` (namespaced pages). A first write uses `If-Match: none`.

## Run (co-located with a cluster peer, like store-publishd)
```sh
# cert: any TLS cert/key (self-signed is fine; callers pin via --cacert)
openssl req -x509 -newkey ed25519 -nodes -days 3650 -subj "/CN=store-wiki" \
  -keyout tls/key.pem -out tls/cert.pem
printf '%s' "$WIKI_TOKEN" > wiki.token
store-wiki --listen 0.0.0.0:8444 --cert tls/cert.pem --key tls/key.pem \
  --token-file wiki.token --index 127.0.0.1:9700 --holder 127.0.0.1:9710[,<peer2>:9710,...] [--max-body-mb 16]
```
`--index` = a cluster peer's mesh port (authors the `Put`); `--holder` = holders (comma list = RF). Run one
per store node for multi-node reads (edit on A is visible on B once the index Put gossips + the blob replicates).

## Proven (local E2E)
PUT new → GET → PUT v2 (If-Match) → GET v2 → stale PUT → 409+current → history (parent chain + ts/author) →
GET `@<old-hash>` exact version → bad-token 401, /health 200. Multi-node consistency is inherited from the
store's proven mesh index replication + holder RF.

## Build
Member of the repo-root cargo workspace; `nix build .#default` (root) builds it with `store` + `store-publishd`
static-musl, or `cargo build --release -p store-wiki`. Reuses the store CLI's mesh + content client (inlined;
follow-up: extract a shared `store-client` lib — the client is now duplicated across cli/publishd/wiki).

## v2 (deferred): per-page sequence-CRDT app-SM for browser char-level live editing (mesh-dev sketches the
position-id/tombstone/fold; store-dev does the snapshot/persistence + catch-up). Clean v1→v2, no rework.
