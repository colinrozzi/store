# content-node -- content transport (D2): fetch-by-hash over tcp

The store's content-transport actor (see `../CONTENT-TRANSPORT-DESIGN.md`): a box missing
a hash fetches the bytes from a peer over its **own tcp** (not the mesh node's DAG
transport) and **verifies the SHA-256 on receipt**, into the local theater content store
(the same CAS `content-store/` wraps). This is what makes the content layer
*fleet-distributed* rather than per-box.

## v0a -- GREEN (fetch-by-hash between two boxes, live)

Self-orchestrating proof (no external driver): a **server** seeds content + serves
`REQ_GET`; a **client** dials the server on init, requests a hash it lacks, and on the
`BLOB` reply re-verifies the SHA-256 and shuts down with `content-node-fetch-passed`.
Proven end-to-end under theater@00b0bf93 -- the requested hash matched `sha256sum` of the
seed, and the client verified it after a real tcp hop:
```
[server] listening 127.0.0.1:9801, holds 1102d753...b413
[client] REQ_GET 1102d753...b413 -> 127.0.0.1:9801
[server] served BLOB 1102d753...b413 (34 bytes)
[client] BLOB 1102d753...b413 verified (34 bytes)   ->   content-node-fetch-passed
```

## v0b -- GREEN (replicate-on-put to a holder roster, live)

A `role=primary` node puts a blob and PUSHes it to a comma-separated `holders` roster
(RF = roster size; content is immutable so replication needs no coordination -- just copy).
Each holder verifies the SHA-256, stores, and ACKs. Proven end-to-end: an EMPTY holder
receives a PUSH from the primary, then a separate client fetches that hash FROM THE HOLDER
(not the primary) and verifies it -- so the holder served content it only obtained via
replication.

## Wire protocol
Self-framed, one connection carries many frames: `[op:u8][hash:32 raw sha256][len:u32 BE][payload]`.
- `op 1` REQ_GET (empty payload) -- requester asks a holder for a hash
- `op 2` BLOB (payload = bytes)  -- holder returns the bytes
- `op 3` MISS (empty)            -- holder does not hold it
- `op 4` PUSH (payload = bytes)  -- primary replicates a blob to a holder
- `op 5` ACK (empty)             -- holder confirms it stored the replica

Every BLOB/PUSH is SHA-256-verified against its hash on receipt (dropped on mismatch).
Content is immutable, so no version / order / read-auth is needed -- integrity is the hash.

## Run (spawn processes + a wait)
See `server.toml.example` / `client.toml.example` (handlers: self/tcp/store). Config is a
tiny string:
- server/holder: `role=server;listen=ADDR[;seed=TEXT]` (omit seed = empty holder)
- primary:       `role=primary;seed=TEXT;holders=ADDR1,ADDR2`
- client:        `role=client;peer=ADDR;expect=<64-hex sha256>`

Spawn holders/servers, wait for their ports, spawn the primary (replicates), then the
client (fetches + self-verifies, exits with the pass marker). `expect` = `sha256(seed)`.

## Next (v1, per the design)
- membership-seeded peer candidates (`Session::members()` + allow-list) + registry addresses,
- GC-by-liveness: drop local content not referenced by the live index (`current-state`) --
  the index-live GC root; quorum-aware (a holder keeps index-live hashes; edges GC by LRU/TTL),
- liveness-aware quorum (count only holders that answer) -- all content-layer-owned.
