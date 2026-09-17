# content-node — content transport (D2 v0a): fetch-by-hash over tcp

The store's content-transport actor (see `../CONTENT-TRANSPORT-DESIGN.md`): a box missing
a hash fetches the bytes from a peer over its **own tcp** (not the mesh node's DAG
transport) and **verifies the SHA-256 on receipt**, into the local theater content store
(the same CAS `content-store/` wraps). This is what makes the content layer
*fleet-distributed* rather than per-box.

## v0a — GREEN (fetch-by-hash between two boxes, live)

Self-orchestrating proof (no external driver): a **server** seeds content + serves
`REQ_GET`; a **client** dials the server on init, requests a hash it lacks, and on the
`BLOB` reply re-verifies the SHA-256 and shuts down with `content-node-fetch-passed`.
Proven end-to-end under theater@00b0bf93 — the requested hash matched `sha256sum` of the
seed, and the client verified it after a real tcp hop:
```
[server] listening 127.0.0.1:9801, holds 1102d753…b413
[client] REQ_GET 1102d753…b413 -> 127.0.0.1:9801
[server] served BLOB 1102d753…b413 (34 bytes)
[client] BLOB 1102d753…b413 verified (34 bytes)   →   content-node-fetch-passed
```

## Wire protocol
Self-framed, one connection carries many frames: `[op:u8][hash:32 raw sha256][len:u32 BE][payload]`.
`op 1` REQ_GET (empty payload) · `op 2` BLOB (payload = bytes) · `op 3` MISS. Every BLOB is
SHA-256-verified against its hash on receipt (drop on mismatch) — content is immutable, so
no version/order/read-auth is needed.

## Run (two `theater spawn`s + a wait)
See `server.toml.example` / `client.toml.example` (handlers: self/tcp/store; config is a
tiny `role=…;listen=…;seed=…` / `role=client;peer=…;expect=<64-hex sha256>` string). Spawn
the server, wait for its port, spawn the client; the client self-verifies and exits with the
pass marker. `expect` = `sha256(seed)` (the client fetches exactly what the server holds).

## Next (v0b/v1, per the design)
- v0b: replicate-on-put (`PUSH` to a fixed RF over the content layer's own holder roster).
- v1: membership-seeded peers + GC-by-liveness (drop content not referenced by the live index).
