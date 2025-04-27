use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

const PEER_INACTIVITY_TIMEOUT_SECS: u64 = 60;
const HOST_TIMEOUT_SECS: u64 = 30;
const ROOM_IDLE_TIMEOUT_SECS: u64 = 60;
const CHECK_ROOMS_INTERVAL: Duration = Duration::from_secs(5);

pub struct Coordinator {
    rooms: Arc<Mutex<HashMap<String, RoomInfo>>>,  // invite_code -> RoomInfo
}

pub struct RoomInfo {
    pub host: String,
    pub peers: HashMap<String, Instant>,   // peer_addr -> last_activity
    pub last_room_activity: Instant,
    pub last_host_activity: Instant,
}

impl Coordinator {
    pub fn new() -> Self {
        Coordinator {
            rooms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self, bind_addr: &str, main_server_addr: &str) {
        let listener = TcpListener::bind(bind_addr).await.expect(&format!("failed to bind coordinator to {}", bind_addr));
        println!("[Coordinator] listening on {}", bind_addr);

        let rooms_clone = self.rooms.clone();
        let main_server = format!("http://{}/cleanup_room", main_server_addr).clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(CHECK_ROOMS_INTERVAL).await;

                let mut rooms = rooms_clone.lock().unwrap();
                let now = Instant::now();
                let mut rooms_to_delete = Vec::new();

                for (room_id, room_info) in rooms.iter_mut() {
                    let mut peers_to_remove = Vec::new();
                    for (peer, last_seen) in &room_info.peers {
                        if now.duration_since(*last_seen).as_secs() > PEER_INACTIVITY_TIMEOUT_SECS {
                            peers_to_remove.push(peer.clone());
                        }
                    }

                    for peer in &peers_to_remove {
                        println!("[Coordinator] disconnecting inactive peer {} from room {}", peer, room_id);
                        room_info.peers.remove(peer);
                    }

                    if !room_info.peers.contains_key(&room_info.host) {
                        if now.duration_since(room_info.last_host_activity).as_secs() > HOST_TIMEOUT_SECS {
                            println!("[Coordinator] closing room {} because host is missing.", room_id);
                            rooms_to_delete.push(room_id.clone());
                        }
                    } else if now.duration_since(room_info.last_room_activity).as_secs() > ROOM_IDLE_TIMEOUT_SECS {
                        println!("[Coordinator] closing room {} due to inactivity.", room_id);
                        rooms_to_delete.push(room_id.clone());
                    }
                }

                for room_id in rooms_to_delete {
                    rooms.remove(&room_id);
                    let main_server = main_server.clone();
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        let room_id = room_id.clone();
                        let _ = client.post(main_server)
                            .json(&serde_json::json!({ "room_id": room_id }))
                            .send()
                            .await;
                    });
                }
            }
        });

        loop {
            let (mut socket, _) = listener.accept().await.expect("accept failed");
            let rooms_clone = self.rooms.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.expect("read failed");
                let request = String::from_utf8_lossy(&buf[..n]);
                let mut parts = request.split(',');
                let invite_code = parts.next().unwrap().to_string();
                let addr = parts.next().unwrap().to_string();
                let new_room_flag = parts.next().map(|s| s == "true").unwrap_or(false);

                let response = {
                    let mut rooms = rooms_clone.lock().unwrap();

                    if let Some(room_info) = rooms.get_mut(&invite_code) {
                        if !room_info.peers.contains_key(&addr) {
                            println!("[Coordinator] new peer registered for room {}: {}", invite_code, addr);

                            if !room_info.peers.is_empty() {
                                println!("[Coordinator] current peers in room {}: {:?}", invite_code, room_info.peers.keys());
                                println!("[Coordinator] expect hole punching: new peer {} will try to connect to {:?}", addr, room_info.peers.keys());
                            } else {
                                println!("[Coordinator] first peer in room {}. waiting for others...", invite_code);
                            }
                        }

                        room_info.peers.insert(addr.clone(), Instant::now());
                        room_info.last_room_activity = Instant::now();
                        if room_info.host == addr {
                            room_info.last_host_activity = Instant::now();
                        }

                        let known_peers: Vec<String> = room_info.peers.keys()
                            .filter(|k| *k != &addr)
                            .cloned()
                            .collect();
                        serde_json::to_string(&known_peers).unwrap().into_bytes()
                    } else {
                        if new_room_flag {
                            println!("[Coordinator] no existing room {} found. creating new room with {} as host.", invite_code, addr);

                            let mut peers = HashMap::new();
                            peers.insert(addr.clone(), Instant::now());

                            rooms.insert(invite_code.clone(), RoomInfo {
                                host: addr.clone(),
                                peers,
                                last_room_activity: Instant::now(),
                                last_host_activity: Instant::now(),
                            });

                            serde_json::to_string(&Vec::<String>::new()).unwrap().into_bytes()
                        } else {
                            println!("[Coordinator] peer {} tried to refresh but room {} is closed.", addr, invite_code);
                            serde_json::to_string(&"room_closed").unwrap().into_bytes()
                        }
                    }
                };

                let _ = socket.write_all(&response).await;
            });
        }
    }
}
