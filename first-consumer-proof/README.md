# first-consumer-proof -- distribute + boot-serve a wasm (the charter's measure)

The store's north-star, proven end-to-end -- composing Layer 1 (the index SM) and Layer 2
(content transport) into the actual use case:

1. **Publish** a real wasm blob to a content holder (over the content wire, op PUSH),
2. **Register** `name -> content-hash` in the mesh index (author a `Put`),
3. **Consumer boot**: RESOLVE the name via the index's `current-state` -> hash, then FETCH
   the bytes by hash from the holder and VERIFY the SHA-256.

Green under theater@00b0bf93: published a 202,467-byte wasm (`content_node.wasm` as the
package), registered `wasm/inbox-acceptor`, and a consumer resolved the name -> hash via the
index and fetched + verified all 202,467 bytes from the holder (`content-node-fetch-passed`).
This is "a box gets a pinned wasm, by name, verified" -- the http-pull boot-durability gap
closed by the store instead of GitHub.

## Run
`COMPOSITE=<mesh_store.wasm> CONTENT=<content_node.wasm> [PACKAGE=<any wasm>] cargo run --release`
(theater@00b0bf93 on THEATER_BIN's path; mesh-testkit from your mesh checkout).
