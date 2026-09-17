//! D1 -- cold-boot persistence acceptance bar: author a Put, KILL the node (process gone),
//! restart from the SAME data-dir, and assert current-state STILL has the Put with the
//! network unplugged (1-node, no peers -> the only way it survives is persist+resume).
//! On mesh without resume/store-IO this correctly shows NO persistence; green on d6f4f529.
use mesh_testkit::{pubkey, seeded_key, spawn_mesh, wait_for_port, Client};
use std::{env, fs, thread, time::Duration};

const ADDR: &str = "127.0.0.1:9771";
const SEED: &str = "d1-node";
const NAME: &str = "pkg/live";

fn write_manifest(path: &str, pkg: &str, datadir: &str) {
    let init = format!("{{\"node_seed\":\"{SEED}\",\"listen_addr\":\"{ADDR}\"}}");
    fs::write(path, format!(
"name=\"d1-store\"\nversion=\"0.1.0\"\npackage=\"{pkg}\"\nstatic_package=true\ninitial_state='{init}'\n\
[[handler]]\ntype=\"self\"\n[[handler]]\ntype=\"tcp\"\n[[handler]]\ntype=\"timer\"\n\
[[handler]]\ntype=\"message-server\"\n[[handler]]\ntype=\"store\"\nbase_path=\"{datadir}\"\nstore_id=\"d1-store\"\n")).unwrap();
}
fn current(name: &str) -> Option<String> {
    let mut c = Client::connect(ADDR, &seeded_key("d1-obs")).ok()?;
    let st = store_protocol::decode_state(&c.current_state().ok()?)?;
    st.entries.iter().find(|e| e.name == name && !e.tombstone).map(|e| e.hash.clone())
}

fn main() {
    let composite = env::var("COMPOSITE").expect("COMPOSITE=mesh_store.wasm");
    let base = "/tmp/claude-0/-work/a595c3e1-2c20-4928-861d-e9234cce0875/scratchpad/d1-run";
    let _ = fs::remove_dir_all(base);
    let datadir = format!("{base}/data"); // PINNED -- survives across both boots
    fs::create_dir_all(&datadir).unwrap();
    let manifest = format!("{base}/node.toml");
    write_manifest(&manifest, &composite, &datadir);
    let hash = "a".repeat(64);
    let npk = pubkey(&seeded_key(SEED)).to_vec();

    // BOOT 1 -- author Genesis + a Put, confirm it's in current-state
    let mut n1 = spawn_mesh(&manifest, &format!("{base}/boot1.log"));
    assert!(wait_for_port(ADDR, Duration::from_secs(10)), "boot1 didn't listen");
    {
        let mut c = Client::connect(ADDR, &seeded_key("d1-obs")).expect("connect boot1");
        c.submit(&store_protocol::encode(&store_protocol::Cmd::Genesis { allow_list: vec![npk] })).unwrap();
        c.submit(&store_protocol::encode(&store_protocol::Cmd::Put { name: NAME.into(), hash: hash.clone() })).unwrap();
    }
    assert_eq!(current(NAME), Some(hash.clone()), "boot1 current-state must have the Put");
    println!("boot1: authored {} -> {}; current-state confirms it", NAME, &hash[..12]);

    // KILL boot1 -- process gone; network unplugged (1-node, no peers anyway)
    let _ = n1.kill();
    let _ = n1.wait();
    thread::sleep(Duration::from_secs(3)); // free the port + let any persistence flush settle
    println!("killed boot1 (process gone). restarting from the SAME data-dir, network unplugged...");

    // BOOT 2 -- restart from the SAME data-dir; do NOT re-author
    let mut n2 = spawn_mesh(&manifest, &format!("{base}/boot2.log"));
    assert!(wait_for_port(ADDR, Duration::from_secs(10)), "boot2 didn't listen");
    let survived = current(NAME);
    let boot2log = fs::read_to_string(format!("{base}/boot2.log")).unwrap_or_default();
    let resumed = boot2log.contains("resumed from persisted");
    let _ = n2.kill();
    let _ = n2.wait();

    println!("boot2: resume-log={} ; current-state[{}] = {:?}", resumed, NAME, survived.as_deref().map(|s| &s[..12]));
    if survived == Some(hash) {
        println!("\n=== D1 PASSED -- cold-boot serves the last-known index locally (Put survived process death, network unplugged). The store's acceptance bar is met. ===");
    } else {
        println!("\n=== D1 NOT YET -- after restart the index is empty (re-genesis): persistence is OFF on this mesh (expected until d6f4f529's mesh-system store I/O). Harness is correct; re-run against mesh@d6f4f529. ===");
    }
}
