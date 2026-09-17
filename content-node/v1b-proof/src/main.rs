//! v1b — the content layer's GC live-set sourced from a REAL store index.
//! Author Put(name -> content-hash) into a live mesh_store index, read current-state back,
//! derive the live-hash set, then drive a content-node's GC with it. Proves "index = GC root"
//! end-to-end: content the index references is kept; content it doesn't is dropped.
use mesh_testkit::{pubkey, seeded_key, spawn_mesh, wait_for_port, Client};
use sha2::{Digest, Sha256};
use std::{env, fs, thread, time::Duration};

fn hx(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }
fn h(s: &str) -> String { hx(&Sha256::digest(s.as_bytes())) }

fn manifest(path: &str, pkg: &str, init: &str, store_id: &str, dir: &str, tcp: bool, timer_ms: bool) {
    fs::create_dir_all(dir).unwrap();
    let mut m = format!("name = \"{store_id}\"\nversion = \"0.1.0\"\npackage = \"{pkg}\"\nstatic_package = true\ninitial_state = '{init}'\n[[handler]]\ntype = \"self\"\n");
    if tcp { m.push_str("[[handler]]\ntype = \"tcp\"\n"); }
    if timer_ms { m.push_str("[[handler]]\ntype = \"timer\"\n[[handler]]\ntype = \"message-server\"\n"); }
    m.push_str(&format!("[[handler]]\ntype = \"store\"\nbase_path = \"{dir}\"\nstore_id = \"{store_id}\"\n"));
    fs::write(path, m).unwrap();
}

fn main() {
    let composite = env::var("COMPOSITE").expect("COMPOSITE=mesh_store.wasm");
    let content = env::var("CONTENT").expect("CONTENT=content_node.wasm");
    let base = "/tmp/claude-0/-work/a595c3e1-2c20-4928-861d-e9234cce0875/scratchpad/v1b-run";
    let _ = fs::remove_dir_all(base);
    let seed = "store-gc-index";
    let addr = "127.0.0.1:9751";
    let node_pk = pubkey(&seeded_key(seed)).to_vec();

    // 1) spawn the store INDEX node (mesh_store composite) and author names -> content hashes.
    let idxman = format!("{base}/index.toml");
    manifest(&idxman, &composite, &format!("node_seed={seed}"), "gc-index", &format!("{base}/idx"), true, true);
    // node_seed config is JSON for mesh-system; fix the init string:
    fs::write(&idxman, fs::read_to_string(&idxman).unwrap()
        .replace(&format!("node_seed={seed}"), &format!("{{\"node_seed\":\"{seed}\",\"listen_addr\":\"{addr}\"}}"))).unwrap();
    let mut idx = spawn_mesh(&idxman, &format!("{base}/idx.log"));
    assert!(wait_for_port(addr, Duration::from_secs(10)), "index node did not listen");
    let mut c = Client::connect(addr, &seeded_key("gc-observer")).expect("connect index");
    c.submit(&store_protocol::encode(&store_protocol::Cmd::Genesis { allow_list: vec![node_pk] })).expect("genesis");
    // the index references alpha + gamma (their content hashes); beta is NOT referenced.
    c.submit(&store_protocol::encode(&store_protocol::Cmd::Put { name: "pkg/alpha".into(), hash: h("alpha") })).expect("put alpha");
    c.submit(&store_protocol::encode(&store_protocol::Cmd::Put { name: "pkg/gamma".into(), hash: h("gamma") })).expect("put gamma");

    // 2) read current-state -> the LIVE hash set (the GC root).
    let st = store_protocol::decode_state(&c.current_state().expect("current-state")).expect("decode IndexState");
    let live: Vec<String> = st.entries.iter().filter(|e| !e.tombstone).map(|e| e.hash.clone()).collect();
    println!("index current-state -> {} live hashes: {:?}", live.len(), live);
    assert!(live.contains(&h("alpha")) && live.contains(&h("gamma")) && !live.contains(&h("beta")),
        "index should reference alpha+gamma, not beta");

    // 3) drive a content-node's GC with the index-derived live set.
    let gcman = format!("{base}/gc.toml");
    let init = format!("role=gc;seeds=alpha,beta,gamma;live={}", live.join(","));
    manifest(&gcman, &content, &init, "gc-node", &format!("{base}/gc"), false, false);
    let mut cn = spawn_mesh(&gcman, &format!("{base}/gc.log"));
    let mut passed = false;
    for _ in 0..50 {
        if fs::read_to_string(format!("{base}/gc.log")).unwrap_or_default().contains("content-node-gc-passed") { passed = true; break; }
        thread::sleep(Duration::from_millis(200));
    }
    let log = fs::read_to_string(format!("{base}/gc.log")).unwrap_or_default();
    let _ = cn.kill(); let _ = idx.kill();
    assert!(passed, "content-node GC did not pass. log:\n{}", log);
    for l in log.lines().filter(|l| l.contains("[gc]")) { println!("{}", l); }
    println!("\n=== v1b PASSED: real index current-state -> live set -> content GC kept alpha+gamma, dropped beta ===");
}
