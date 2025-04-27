use std::env;
use std::sync::Arc;
use network_hole_punching::peer::{GamePacket, PacketType, Peer};
use tokio::net::TcpStream;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use reqwest::Client;
use serde_json::json;
use std::io::{self, Write};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 4 {
        eprintln!("Usage: {} <bind_addr> <coordinator_addr> <main_server_addr> [--host]", args[0]);
        return;
    }

    let bind_addr = &args[1];
    let coordinator_addr = &args[2];
    let main_server_addr = &args[3];
    let is_host = args.get(4) == Some(&"--host".to_string());

    let client = Client::new();

    let (player_name, room_id, password, token) = if is_host {
        println!("hosting new room...");
        let player_name = ask("Enter your player name: ");
        let password = ask("Set a room password: ");

        let res = client.post(format!("http://{}/create_room", main_server_addr))
            .json(&json!({
                "player_name": player_name,
                "password": password
            }))
            .send()
            .await
            .expect("failed to create room");

        let json: serde_json::Value = res.json().await.expect("invalid response");
        let room_id = json["room_id"].as_str().unwrap().to_string();
        let token = json["token"].as_str().unwrap().to_string();

        println!("room created with ID: {}", room_id);

        (player_name, room_id, password, token)
    } else {
        println!("joining existing room...");

        loop {
            let res = client.get(format!("http://{}/list_rooms", main_server_addr)).send().await;
            if let Ok(response) = res {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    if let Some(rooms) = json["rooms"].as_array() {
                        if rooms.is_empty() {
                            println!("no rooms available.");
                        } else {
                            println!("available rooms:");
                            for room in rooms {
                                println!(" - {}", room.as_str().unwrap());
                            }
                        }
                    }
                }
            }

            let room_id = ask("Enter room ID: ");
            let player_name = ask("Enter your player name: ");
            let password = ask("Enter room password: ");

            let res = client.post(format!("http://{}/join_room", main_server_addr))
                .json(&json!({
                    "room_id": room_id,
                    "player_name": player_name,
                    "password": password
                }))
                .send()
                .await;

            match res {
                Ok(response) if response.status().is_success() => {
                    let json: serde_json::Value = response.json().await.expect("invalid response");
                    let token = json["token"].as_str().unwrap().to_string();
                    break (player_name, room_id, password, token);
                }
                _ => {
                    println!("failed to join room. try again.");
                }
            }
        }
    };

    let peer = Arc::new(Peer::new(bind_addr, &password, token).await);

    let mut stream = TcpStream::connect(coordinator_addr).await.expect("failed to connect to coordinator");
    let my_addr = peer.socket.local_addr().unwrap().to_string();
    let registration = format!("{},{}", room_id, my_addr);
    stream.write_all(registration.as_bytes()).await.expect("failed to send registration");

    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.expect("failed to read coordinator response");
    let response = String::from_utf8_lossy(&buf[..n]);
    println!("coordinator response: {}", response);

    if response != "waiting" {
        let peer_list: Vec<String> = serde_json::from_str(&response).unwrap_or_default();
        for addr in peer_list {
            peer.add_peer(addr).await;
        }
    } else {
        println!("waiting for a peer to join...");
    }

    peer.clone().start_heartbeat().await;
    peer.clone().watch_heartbeat().await;
    peer.clone().start_peer_list_refresher(room_id.clone(), coordinator_addr.clone()).await;
    peer.clone().send_hello().await;

    let peer_clone = peer.clone();
    tokio::spawn(async move {
        peer_clone.recv_loop().await;
    });

    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    println!("You can now type messages to send:");

    while let Ok(Some(line)) = reader.next_line().await {
        let packet = GamePacket {
            packet_type: PacketType::Chat,
            payload: line,
            sender: player_name.clone(),
        };
        peer.send_game_packet(packet).await;
    }
}

fn ask(prompt: &str) -> String {
    print!("{}", prompt);
    io::stdout().flush().unwrap();
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).unwrap();
    buffer.trim().to_string()
}
