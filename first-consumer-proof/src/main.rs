//! FIRST CONSUMER -- "distribute + boot-serve a wasm" (the charter's measure).
//! Publish a real wasm blob to a content holder, register name->hash in the index, then a
//! consumer RESOLVES the name via the index -> FETCHES the bytes by hash -> VERIFIES. This
//! composes Layer 1 (index SM) + Layer 2 (content-node fetch) into the actual store use case.
use mesh_testkit::{pubkey, seeded_key, spawn_mesh, wait_for_port, Client};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::{env, fs, thread, time::Duration};

fn hx(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }

fn manifest(path: &str, pkg: &str, init: &str, id: &str, dir: &str, tcp: bool, mesh: bool) {
    fs::create_dir_all(dir).unwrap();
    let mut m = format!("name=\"{id}\"\nversion=\"0.1.0\"\npackage=\"{pkg}\"\nstatic_package=true\ninitial_state='{init}'\n[[handler]]\ntype=\"self\"\n");
    if tcp { m.push_str("[[handler]]\ntype=\"tcp\"\n"); }
    if mesh { m.push_str("[[handler]]\ntype=\"timer\"\n[[handler]]\ntype=\"message-server\"\n"); }
    m.push_str(&format!("[[handler]]\ntype=\"store\"\nbase_path=\"{dir}\"\nstore_id=\"{id}\"\n"));
    fs::write(path, m).unwrap();
}
fn wait_log(path: &str, needle: &str) -> bool {
    for _ in 0..60 {
        if fs::read_to_string(path).unwrap_or_default().contains(needle) { return true; }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

fn main() {
    let composite = env::var("COMPOSITE").expect("COMPOSITE=mesh_store.wasm");
    let content = env::var("CONTENT").expect("CONTENT=content_node.wasm");
    let pkg_path = env::var("PACKAGE").unwrap_or_else(|_| content.clone()); // ship content_node.wasm as "the package"
    let base = "/tmp/claude-0/-work/a595c3e1-2c20-4928-861d-e9234cce0875/scratchpad/fc-run";
    let _ = fs::remove_dir_all(base);

    let wasm = fs::read(&pkg_path).expect("read package wasm");
    let hash = Sha256::digest(&wasm).to_vec();
    let hash_hex = hx(&hash);
    let name = "wasm/inbox-acceptor";
    println!("package {} = {} bytes, sha256 {}", pkg_path, wasm.len(), hash_hex);

    // 1) a content HOLDER (empty) that will hold the published wasm
    let haddr = "127.0.0.1:9761";
    manifest(&format!("{base}/holder.toml"), &content, &format!("role=server;listen={haddr}"), "fc-holder", &format!("{base}/h"), true, false);
    let mut holder = spawn_mesh(&format!("{base}/holder.toml"), &format!("{base}/holder.log"));
    assert!(wait_for_port(haddr, Duration::from_secs(10)), "holder didn't listen");

    // 2) PUBLISH: push the wasm bytes to the holder over the content wire ([op=4 PUSH][hash32][len][bytes])
    let mut frame = vec![4u8];
    frame.extend_from_slice(&hash);
    frame.extend_from_slice(&(wasm.len() as u32).to_be_bytes());
    frame.extend_from_slice(&wasm);
    let mut s = TcpStream::connect(haddr).expect("connect holder");
    s.write_all(&frame).expect("push wasm");
    let mut ack = [0u8; 64];
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = s.read(&mut ack); // best-effort ACK
    assert!(wait_log(&format!("{base}/holder.log"), "stored PUSH"), "holder did not store the pushed wasm");
    println!("published: holder stored the wasm ({} bytes)", wasm.len());

    // 3) REGISTER name -> hash in the index
    let iaddr = "127.0.0.1:9762";
    let seed = "fc-index";
    let npk = pubkey(&seeded_key(seed)).to_vec();
    manifest(&format!("{base}/index.toml"), &composite, &format!("{{\"node_seed\":\"{seed}\",\"listen_addr\":\"{iaddr}\"}}"), "fc-index", &format!("{base}/i"), true, true);
    let mut idx = spawn_mesh(&format!("{base}/index.toml"), &format!("{base}/index.log"));
    assert!(wait_for_port(iaddr, Duration::from_secs(10)), "index didn't listen");
    let mut c = Client::connect(iaddr, &seeded_key("fc-obs")).expect("connect index");
    c.submit(&store_protocol::encode(&store_protocol::Cmd::Genesis { allow_list: vec![npk] })).unwrap();
    c.submit(&store_protocol::encode(&store_protocol::Cmd::Put { name: name.into(), hash: hash_hex.clone() })).unwrap();
    println!("registered {} -> {}", name, hash_hex);

    // 4) CONSUMER boot: RESOLVE the name via the index, then FETCH the bytes by hash + verify.
    let st = store_protocol::decode_state(&c.current_state().unwrap()).unwrap();
    let resolved = st.entries.iter().find(|e| e.name == name && !e.tombstone).map(|e| e.hash.clone())
        .expect("index resolves the name");
    assert_eq!(resolved, hash_hex, "resolved hash matches the published content hash");
    println!("resolved {} -> {} (via index current-state)", name, resolved);

    manifest(&format!("{base}/consumer.toml"), &content, &format!("role=client;peer={haddr};expect={resolved}"), "fc-consumer", &format!("{base}/c"), true, false);
    let mut consumer = spawn_mesh(&format!("{base}/consumer.toml"), &format!("{base}/consumer.log"));
    let ok = wait_log(&format!("{base}/consumer.log"), "content-node-fetch-passed");
    let clog = fs::read_to_string(format!("{base}/consumer.log")).unwrap_or_default();
    let _ = consumer.kill(); let _ = idx.kill(); let _ = holder.kill();
    assert!(ok, "consumer failed to resolve+fetch+verify. log:\n{}", clog);
    for l in clog.lines().filter(|l| l.contains("[client]")) { println!("{}", l); }
    println!("\n=== FIRST CONSUMER PASSED ===");
    println!("Published a {}-byte wasm, registered it by name in the index, and a consumer", wasm.len());
    println!("resolved the name -> hash via the index and fetched+verified the bytes from a holder.");
}
