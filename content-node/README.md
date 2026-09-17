# content-node -- content transport (D2 v0a): fetch-by-hash over tcp

The store's content-transport actor (see `../CONTENT-TRANSPORT-DESIGN.md`): a box missing
a hash fetches the bytes from a peer over its **own tcp** (not the mesh node's DAG
transport) and **verifies the SHA-256 on receipt**, into the local theater content store
(the same CAS `content-store/` wraps). This is what makes the content layer
*fleet-distributed* rather than per-box.

## v0a -- GREEN (fetch-by-hash between two boxes, live)

Self-orchestrating proof (no external driver): a **server** seeds content + serves
`REQ_GET`; a **client** dials the server on init, requests a hash it lacks, and on the
`BLOB` reply re-verifies the SHA-256 and shuts down with `content-node-fetch-passed`.
Proven end-to-end under theater@00b0bf93 -- the requested hash matched `sha256sum` of the
seed, and the client verified it after a real tcp hop:
```
[server] listening 127.0.0.1:9801, holds 1102d753--¦b413
[client] REQ_GET 1102d753--¦b413 -> 127.0.0.1:9801
[server] served BLOB 1102d753--¦b413 (34 bytes)
[client] BLOB 1102d753--¦b413 verified (34 bytes)   --   content-node-fetch-passed
```

## Wire protocol
Self-framed, one connection carries many frames: `[op:u8][hash:32 raw sha256][len:u32 BE][payload]`.
`op 1` REQ_GET (empty payload) Â· `op 2` BLOB (payload = bytes) Â· `op 3` MISS. Every BLOB is
SHA-256-verified against its hash on receipt (drop on mismatch) -- content is immutable, so
no version/order/read-auth is needed.

## Run (two `theater spawn`s + a wait)
See `server.toml.example` / `client.toml.example` (handlers: self/tcp/store; config is a
tiny `role=--¦;listen=--¦;seed=--¦` / `role=client;peer=--¦;expect=<64-hex sha256>` string). Spawn
the server, wait for its port, spawn the client; the client self-verifies and exits with the
pass marker. `expect` = `sha256(seed)` (the client fetches exactly what the server holds).

## v0b -- GREEN (replicate-on-put to a holder roster, live)

A `role=primary` node puts a blob and PUSHes it to a comma-separated `holders` roster
(RF = roster size; content is immutable so replication needs no coordination -- just copy).
Each holder verifies the SHA-256, stores, and ACKs. Proven end-to-end: an EMPTY holder
receives a PUSH from the primary, then a separate client fetches that hash FROM THE HOLDER
(not the primary) and verifies it -- so the holder served content it only obtained via
replication. `PUSH` (op 4) / `ACK` (op 5) added to the wire.

## Next (v1, per the design)
- membership-seeded peer candidates (`Session::members()` + allow-list) + registry addresses,
- GC-by-liveness: drop local content not referenced by the live index (`current-state`) —
  the index-live GC root; quorum-aware (a holder keeps index-live hashes; edges GC by LRU/TTL),
- liveness-aware quorum (count only holders that answer) -- all content-layer-owned.
