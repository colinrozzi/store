# store — the fleet content + config layer

## Mission
**"The store" is the fleet's foundational distribution primitive:** a generic, fleet-wide
`name → value` layer that everything else stands on. Package distribution, secrets, manifests,
and CLI-tool delivery are all *applications built on the store* — not bespoke mechanisms.

Owner: **store-dev@colinrozzi.com**

## Why it exists
The fleet keeps re-solving one problem in four different ways — *publish something from our side,
have every box get it, verified and durable*:
- **wasm** → the http-pull path
- **config / manifests** → the roster + manifest dance
- **secrets** → the on-box store-populate step
- **CLI tools** → fleet-release + checksummed upgrade

The store collapses those into **one primitive**; the four become conventions on `put`/`get`.
(Directly motivated by the 2026-09 inbox cutover + http-pull work: theater's package fetch is
in-memory only, so a pure-http boot depends on GitHub — the store is the durable, boot-safe,
layer-clean answer. Colin's call: build the *primitive*, not the specific apps.)

## The primitive (deliberately dumb)
- `put(name, bytes)` / `get(name) → bytes`, fleet-wide.
- Writable from a central, **authorized** point; readable **LOCALLY on every box** — boot-safe:
  no network at read time.
- Content-addressed + digest-verified by construction.

Keep Layer 0 generic (names + bytes). Content-addressing, encryption, schema are **app choices**
(Layer 1), never baked into the primitive.

## Boundary
- **mesh-dev** stays focused on the mesh itself. **store-dev** builds the store *as a mesh app* on top.
- The store is a mesh app: it uses the mesh for propagation + identity, and each node's local
  theater store as its on-box substrate.

See DESIGN.md for the two-layer model and the open questions.
