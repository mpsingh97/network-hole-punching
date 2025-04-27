use std::{sync::Arc, time::{Duration, Instant}, collections::HashSet};

use aes_gcm::{aead::{rand_core::RngCore, Aead, OsRng}, Aes256Gcm, KeyInit, Nonce};
use hkdf::Hkdf;
use jsonwebtoken::{decode, errors::Result as JwtResult, Algorithm, DecodingKey, TokenData, Validation};
use sha2::{digest::generic_array::GenericArray, Sha256};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpStream, UdpSocket}, sync::RwLock};

use crate::main_server::{Claims, SECRET};

const NONCE_SIZE: usize = 12;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT_SECS: Duration = Duration::from_secs(30);
const PEERS_LIST_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_MESSAGE: &str = "heartbeat";
const RE_PUNCH_MESSAGE: &str = "re-punch";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum PacketType {
    Move,
    Chat,
    Action,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct GamePacket {
    pub packet_type: PacketType,
    pub payload: String,
    pub sender: String,
}

pub struct Peer {
    pub socket: Arc<UdpSocket>,
    cipher: Aes256Gcm,
    peer_addrs: Arc<RwLock<HashSet<String>>>,
    last_heartbeat: Arc<RwLock<Instant>>,
    pub token: String,
    pub host_addr: Arc<RwLock<Option<String>>>,
}

impl Peer {
    pub async fn new(bind_addr: &str, password: &str, token: String) -> Self {
        let socket = UdpSocket::bind(bind_addr).await.expect(format!("failed to bind peer to {}", bind_addr).as_str());
        let cipher = Self::init_cipher(password);
        Peer {
            socket: Arc::new(socket), 
            cipher, 
            peer_addrs: Arc::new(RwLock::new(HashSet::new())),
            last_heartbeat: Arc::new(RwLock::new(Instant::now())),
            token,
            host_addr: Arc::new(RwLock::new(None)),
        }
    }

    fn init_cipher(password: &str) -> Aes256Gcm {
        let hk = Hkdf::<Sha256>::new(None, password.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(b"aes-key", &mut key).expect("hkdf expand failed");
        Aes256Gcm::new(GenericArray::from_slice(&key))
    }

    pub async fn send_encrypted(&self, plaintext: &[u8]) {
        let peer_addrs = self.peer_addrs.read().await;
        for addr in peer_addrs.iter() {
            let mut nonce_bytes = [0u8; NONCE_SIZE];
            OsRng.fill_bytes(&mut nonce_bytes);
            let nonce = Nonce::from_slice(&nonce_bytes);

            let ciphertext = self.cipher.encrypt(nonce, plaintext).expect("encryption failed");
            let mut packet = nonce_bytes.to_vec();
            packet.extend(ciphertext);

            self.socket.send_to(&packet, addr).await.expect("failed to send packet");
        }
    }

    pub async fn send_game_packet(&self, packet: GamePacket) {
        let json = serde_json::to_string(&packet).unwrap();
        self.send_encrypted(json.as_bytes()).await;
    }

    pub async fn recv_loop(self: Arc<Self>) {
        let mut buf = vec![0u8; 1500];
        loop {
            let (len, addr) = self.socket.recv_from(&mut buf).await.expect("receive failed");
            
            let packet = &buf[..len];
            if packet.len() < NONCE_SIZE {
                println!("received malformed packet from {}: nonce bytes missing", addr);
                continue;
            }

            let nonce = Nonce::from_slice(&packet[..NONCE_SIZE]);
            let encrypted = &packet[NONCE_SIZE..];

            match self.cipher.decrypt(nonce, encrypted) {
                Ok(plaintext) => {
                    let msg = String::from_utf8_lossy(&plaintext);
                    if msg.starts_with("HELLO ") {
                        let received_token = &msg[6..];
                    
                        match verify_token(received_token) {
                            Ok(data) => {
                                println!("[Handshake] Peer authenticated. Room ID: {}, Player ID: {}", data.claims.room_id, data.claims.player_id);
                            }
                            Err(err) => {
                                println!("[Handshake] Peer authentication failed: {}", err);
                            }
                        }
                    } else if msg == HEARTBEAT_MESSAGE {
                        let mut heartbeat = self.last_heartbeat.write().await;
                        *heartbeat = Instant::now();
                    } else {
                        match serde_json::from_str::<GamePacket>(&msg) {
                            Ok(packet) => {
                                match packet.packet_type {
                                    PacketType::Move => println!("[MOVE] {}: {}", packet.sender, packet.payload),
                                    PacketType::Chat => println!("[CHAT] {}: {}", packet.sender, packet.payload),
                                    PacketType::Action => println!("[ACTION] {}: {}", packet.sender, packet.payload),
                                }
                            }
                            Err(_) => println!("received: {}", msg),
                        }
                    }
                },
                Err(_) => {},
            }
        }
    }

    pub async fn start_heartbeat(self: Arc<Self>) {
        let peer_clone = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                peer_clone.send_encrypted(HEARTBEAT_MESSAGE.as_bytes()).await;
            }
        });
    }

    pub async fn watch_heartbeat(self: Arc<Self>) {
        let peer_clone = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let last = peer_clone.last_heartbeat.read().await;
                if last.elapsed() > HEARTBEAT_TIMEOUT_SECS {
                    println!("peer timeout detected, attempting re-punch...");
                    peer_clone.re_punch().await;
                }
            }
        });
    }

    pub async fn re_punch(&self) {
        let peer_addrs = self.peer_addrs.read().await.clone();
        if !peer_addrs.is_empty() {
            for _ in 0..5 {
                self.send_encrypted(RE_PUNCH_MESSAGE.as_bytes()).await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    pub async fn add_peer(&self, addr: String) {
        let mut peer_addrs = self.peer_addrs.write().await;
        peer_addrs.insert(addr);
    }

    pub async fn start_peer_list_refresher(self: Arc<Self>, invite_code: String, coordinator_addr: String) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PEERS_LIST_REFRESH_INTERVAL).await;
    
                if let Ok(mut stream) = TcpStream::connect(&coordinator_addr).await {
                    let my_addr = self.socket.local_addr().unwrap().to_string();
                    let registration = format!("{},{}", invite_code, my_addr);
                    if stream.write_all(registration.as_bytes()).await.is_ok() {
                        let mut buf = [0u8; 2048];
                        if let Ok(n) = stream.read(&mut buf).await {
                            if n > 0 {
                                let response_text = String::from_utf8_lossy(&buf[..n]);
                                if response_text == "\"disconnected\"" {
                                    println!("[Peer] disconnected by server due to inactivity. exiting...");
                                    std::process::exit(0);
                                } else if response_text == "\"room_closed\"" {
                                    println!("[Peer] room closed by server. exiting...");
                                    std::process::exit(0);
                                } else {
                                    if let Ok(peer_list) = serde_json::from_str::<Vec<String>>(&response_text) {
                                        let mut peer_addrs = self.peer_addrs.write().await;
                                        let old_peers: std::collections::HashSet<_> = peer_addrs.iter().cloned().collect();
                                        for addr in peer_list {
                                            peer_addrs.insert(addr);
                                        }
                                        if *peer_addrs != old_peers {
                                            println!("[Refresher] updated peer list: {:?}", *peer_addrs);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    pub async fn send_hello(&self) {
        let hello_message = format!("HELLO {}", self.token.as_str());
        self.send_encrypted(hello_message.as_bytes()).await;
    }

    pub async fn set_host_addr(&self, addr: String) {
        let mut host_addr = self.host_addr.write().await;
        *host_addr = Some(addr);
    }
}

fn verify_token(token: &str) -> JwtResult<TokenData<Claims>> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(SECRET),
        &Validation::new(Algorithm::HS256),
    )
}