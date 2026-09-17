# v1b-proof -- the content layer's GC live-set from a REAL store index

Proves "the index is the GC root" end-to-end, wiring Layer 1 (the index SM) to Layer 2
(content GC). The driver:
1. spawns a real `mesh_store` index node,
2. authors `Genesis` + `Put(pkg/alpha -> sha256(alpha))` + `Put(pkg/gamma -> sha256(gamma))`
   (beta is deliberately NOT referenced),
3. reads `current-state` back and decodes it to `IndexState` -> the LIVE hash set,
4. drives a `content-node` in `role=gc` with `live=<those hashes>`; it holds alpha/beta/gamma
   and drops every hash not in the live set.

Result (green): the content-node kept alpha + gamma (index-referenced) and dropped beta
(unreferenced) -- `content-node-gc-passed`. The live set genuinely came from authored Puts
read back out of a running index. A native driver bridges index+content here (a realistic
GC-coordinator shape); an in-actor mesh-client query is the productionization step.

## Run
`COMPOSITE=<mesh_store.wasm> CONTENT=<content_node.wasm> cargo run --release`
(theater@00b0bf93 on THEATER_BIN's path; mesh-testkit from your mesh checkout).
