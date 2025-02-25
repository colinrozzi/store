mod bindings;
use bindings::exports::ntwk::theater::actor::Guest as ActorGuest;
use bindings::exports::ntwk::theater::message_server_client::Guest as MessageGuest;
use bindings::ntwk::theater::filesystem::{list_files, read_file, write_file};
use bindings::ntwk::theater::runtime::log;
use bindings::ntwk::theater::types::Json;
use serde::{Deserialize, Serialize};
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
    fn init(_data: Option<Vec<u8>>, _params: (String,)) -> Result<(Option<Vec<u8>>,), String> {
        log("Initializing key-value store");
        let initial_state = State::new();
        //setup_data();
        Ok((Some(serde_json::to_vec(&initial_state).unwrap()),))
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

#[derive(Serialize, Deserialize, Debug)]
struct PutResponse {
    key: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct GetResponse {
    key: String,
    value: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
struct AllResponse {
    data: Vec<GetResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
enum ResponseData {
    Put(PutResponse),
    Get(GetResponse),
    All(AllResponse),
    Error(String),
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    status: String,
    data: ResponseData,
}

impl MessageGuest for Component {
    fn handle_send(
        state: Option<Vec<u8>>,
        params: (Vec<u8>,),
    ) -> Result<(Option<Vec<u8>>,), String> {
        let msg = params.0;
        log(format!(
            "Handling send: {:?}",
            String::from_utf8(msg.clone()).unwrap()
        )
        .as_str());
        log("Send not implemented");
        Ok((state,))
    }

    fn handle_request(
        state: Option<Vec<u8>>,
        params: (Vec<u8>,),
    ) -> Result<(Option<Vec<u8>>, (Vec<u8>,)), String> {
        log("Handling request");
        let msg = params.0;
        log(&format!(
            "Request: {:?}",
            String::from_utf8(msg.clone()).unwrap()
        ));
        let mut state: State = serde_json::from_slice(&state.unwrap()).unwrap();
        let request: Request = serde_json::from_slice(&msg).unwrap();

        #[allow(unused_assignments)]
        let mut response = Response {
            status: "error".to_string(),
            data: ResponseData::Error("Unknown request type".to_string()),
        };

        match request.data {
            Action::Get(key) => {
                if let Some(value) = state.get(&key) {
                    response = Response {
                        status: "ok".to_string(),
                        data: ResponseData::Get(GetResponse {
                            key: key.clone(),
                            value: value.clone(),
                        }),
                    }
                } else {
                    response = Response {
                        status: "error".to_string(),
                        data: ResponseData::Error("Key not found".to_string()),
                    }
                }
            }
            Action::Put(value) => {
                let key = state.put(value);
                response = Response {
                    status: "ok".to_string(),
                    data: ResponseData::Put(PutResponse { key }),
                }
            }
            Action::All(()) => {
                let keys = list_files(".").unwrap();
                let mut data = Vec::new();
                for key in keys {
                    if let Some(value) = state.get(&key) {
                        data.push(GetResponse {
                            key: key.clone(),
                            value: value.clone(),
                        });
                    }
                }
                response = Response {
                    status: "ok".to_string(),
                    data: ResponseData::All(AllResponse { data }),
                }
            }
        }

        Ok((Some(serde_json::to_vec(&response).unwrap()), (msg,)))
    }
}

bindings::export!(Component with_types_in bindings);
