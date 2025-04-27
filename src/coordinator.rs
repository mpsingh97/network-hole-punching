use std::{collections::{HashMap, HashSet}, sync::{Arc, Mutex}};

use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

/*
TODO

Add room closure on
1. Inactibity for certain time
2. If host disconnects and doesn't reconnect in certain time

Disconnect peer if idle for certain time
 */
pub struct Coordinator {
    peers: Arc<Mutex<HashMap<String, HashSet<String>>>>  // invite_code -> set of peer_address
}

impl Coordinator {
    pub fn new() -> Self {
        Coordinator {
            peers: Arc::new(Mutex::new(HashMap::new()))
        }
    }

    pub async fn run(&self, bind_addr: &str) {
        let listener = TcpListener::bind(bind_addr).await.expect(&format!("failed to bind coordinator to {}", bind_addr));

        loop {
            let (mut socket, _) = listener.accept().await.expect("accept failed");
            let peers_clone = self.peers.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.expect("read failed");
                let request = String::from_utf8_lossy(&buf[..n]);
                let mut parts = request.split(',');
                let invite_code = parts.next().unwrap().to_string();
                let addr = parts.next().unwrap().to_string();

                let response = {
                    let mut peers = peers_clone.lock().unwrap();
                    let room_peers = peers.entry(invite_code.clone()).or_insert_with(HashSet::new);

                    if !room_peers.contains(&addr) {
                        println!("[Coordinator] new peer registered for room {}: {}", invite_code, addr);

                        if !room_peers.is_empty() {
                            println!("[Coordinator] current peers in room {}: {:?}", invite_code, room_peers);
                            println!("[Coordinator] expect hole punching: new peer {} will try to connect to {:?}", addr, room_peers);
                        } else {
                            println!("[Coordinator] first peer in room {}. waiting for others...", invite_code);
                        }

                        room_peers.insert(addr.clone());
                    }
                    
                    let known_peers: Vec<String> = room_peers.iter().cloned().collect();
                    room_peers.insert(addr.clone());
                    serde_json::to_string(&known_peers).unwrap().into_bytes()
                };

                let _ = socket.write_all(&response).await;
            });
        }
    }
}