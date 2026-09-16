//! Live driven proof for the store index SM: spawn the composed mesh_store node,
//! author a Genesis + Put over the node, and assert current-state reflects the index.
use mesh_testkit::{pubkey, seeded_key, spawn_mesh, wait_for_port, Client};
use std::time::Duration;
use std::{env, fs};

fn h(c: char) -> String { std::iter::repeat(c).take(64).collect() }

fn main() {
    let composite = env::var("COMPOSITE").expect("COMPOSITE env = mesh_store.wasm path");
    let addr = "127.0.0.1:9731";
    let seed = "store-dev-node-seed-live";
    let node_pk = pubkey(&seeded_key(seed)).to_vec();

    // manifest for a 1-node store network
    let dir = "/tmp/claude-0/-work/a595c3e1-2c20-4928-861d-e9234cce0875/scratchpad/store-driver-run";
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(format!("{dir}/data")).unwrap();
    let manifest = format!("{dir}/manifest.toml");
    let init = format!(r#"{{"node_seed":"{seed}","listen_addr":"{addr}"}}"#);
    fs::write(&manifest, format!(
r#"name = "mesh-store-live"
version = "0.1.0"
package = "{composite}"
static_package = true
initial_state = '{init}'

[[handler]]
type = "self"
[[handler]]
type = "tcp"
[[handler]]
type = "timer"
[[handler]]
type = "message-server"
[[handler]]
type = "store"
base_path = "{dir}/data"
store_id = "mesh-store-live"
"#)).unwrap();

    let mut child = spawn_mesh(&manifest, &format!("{dir}/node.log"));
    assert!(wait_for_port(addr, Duration::from_secs(10)), "node did not listen on {addr}");
    println!("✓ node up on {addr}");

    let mut c = Client::connect(addr, &seeded_key("store-observer")).expect("connect");

    // (a) a Put BEFORE genesis must be REJECTED by validate (store not initialized)
    let early = store_protocol::encode(&store_protocol::Cmd::Put { name: "x".into(), hash: h('a') });
    match c.submit(&early) {
        Err(e) => println!("✓ pre-genesis Put rejected: {e}"),
        Ok(_) => { let _ = child.kill(); panic!("pre-genesis Put should have been rejected"); }
    }

    // (b) Genesis: allow-list the node's own pubkey (node authors + signs with node key)
    let g = store_protocol::encode(&store_protocol::Cmd::Genesis { allow_list: vec![node_pk.clone()] });
    c.submit(&g).expect("submit genesis");
    println!("✓ Genesis authored");

    // (c) an authorized Put (author = node pubkey, now in the allow-list)
    let put = store_protocol::encode(&store_protocol::Cmd::Put { name: "wasm/inbox".into(), hash: h('a') });
    c.submit(&put).expect("submit put");
    let put2 = store_protocol::encode(&store_protocol::Cmd::Put { name: "manifest/acceptor".into(), hash: h('b') });
    c.submit(&put2).expect("submit put2");
    println!("✓ two authorized Puts authored");

    // (d) read current-state and assert it reflects the index
    let bytes = c.current_state().expect("current_state");
    let st = store_protocol::decode_state(&bytes).expect("decode IndexState");
    println!("current-state: genesis_done={} allow_list={} entries={}",
        st.genesis_done, st.allow_list.len(), st.entries.len());
    assert!(st.genesis_done, "genesis applied");
    assert_eq!(st.allow_list, vec![node_pk], "allow-list = [node pubkey]");
    assert_eq!(st.resolve("wasm/inbox"), Some(h('a').as_str()), "put1 resolves");
    assert_eq!(st.resolve("manifest/acceptor"), Some(h('b').as_str()), "put2 resolves");
    assert_eq!(st.resolve("nope"), None, "absent name resolves to None");

    println!("\n=== LIVE PROOF PASSED ===");
    println!("Authored Genesis + 2 Puts through a real composed mesh_store node;");
    println!("current-state reflects the index; pre-genesis write rejected by validate.");
    let _ = child.kill();
}
