mod bindings;
use bindings::exports::ntwk::theater::actor::Guest as ActorGuest;
use bindings::exports::ntwk::theater::message_server_client::Guest as MessageGuest;
use bindings::ntwk::theater::filesystem::{list_files, read_file, write_file};
use bindings::ntwk::theater::runtime::log;
use bindings::ntwk::theater::types::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
struct State {
    cache: HashMap<String, Vec<u8>>,
}

impl State {
    fn new() -> Self {
        State {
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<&Vec<u8>> {
        // first, check if the cache has the key
        // if not, check the file system and cache the result
        if self.cache.contains_key(key) {
            self.cache.get(key)
        } else {
            let value = read_file(key).ok();
            if let Some(value) = value {
                self.cache.insert(key.to_string(), value.clone());
                self.cache.get(key)
            } else {
                None
            }
        }
    }

    fn put(&mut self, value: Vec<u8>) -> String {
        let key = format!("{:x}", Sha1::digest(&value));
        // write to file
        let value_str = String::from_utf8(value.clone()).unwrap();
        write_file(&key, &value_str).unwrap();
        self.cache.insert(key.clone(), value);
        key
    }
}

struct Component;

impl ActorGuest for Component {
    fn init(_data: Option<Vec<u8>>) -> Vec<u8> {
        log("Initializing key-value store");
        let initial_state = State::new();
        //setup_data();
        serde_json::to_vec(&initial_state).unwrap()
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Request {
    _type: String,
    data: Action,
}

#[derive(Serialize, Deserialize, Debug)]
enum Action {
    Get(String),
    Put(Vec<u8>),
    All(()),
}

impl MessageGuest for Component {
    fn handle_send(msg: Json, state: Json) -> Json {
        log(format!(
            "Handling send: {:?}",
            String::from_utf8(msg.clone()).unwrap()
        )
        .as_str());
        log("Send not implemented");
        state
    }

    fn handle_request(msg: Json, state: Json) -> (Json, Json) {
        log("Handling request");
        log(&format!(
            "Request: {:?}",
            String::from_utf8(msg.clone()).unwrap()
        ));
        log(&format!(
            "State: {:?}",
            String::from_utf8(state.clone()).unwrap()
        ));
        let mut state: State = serde_json::from_slice(&state).unwrap();
        log("State deserialized");
        let request: Request = serde_json::from_slice(&msg).unwrap();
        log(&format!("Request: {:?}", request));
        let mut response = json!({"status": "error", "message": "Unknown request type"});

        match request.data {
            Action::Get(key) => {
                if let Some(value) = state.get(&key) {
                    response = json!({"status": "ok", "value": value});
                } else {
                    response = json!({"status": "error", "message": "Key not found"});
                }
            }
            Action::Put(value) => {
                let key = state.put(value);
                response = json!({"status": "ok", "key": key});
            }
            Action::All(()) => {
                let keys = list_files(".").unwrap();
                let mut data = Vec::new();
                for key in keys {
                    if let Some(value) = state.get(&key) {
                        data.push(json!({"key": key, "value": value}));
                    }
                }
                response = json!({"status": "ok", "data": data});
            }
        }

        (
            serde_json::to_vec(&response).unwrap(),
            serde_json::to_vec(&state).unwrap(),
        )
    }
}

bindings::export!(Component with_types_in bindings);
