use argon2::Config;
use axum::{extract::State, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use reqwest::StatusCode;
use serde::{Serialize, Deserialize};
use serde_json::json;
use uuid::Uuid;
use std::{collections::HashMap, net::SocketAddr, sync::{Arc, Mutex}};
use chrono::{Utc, Duration};
use jsonwebtoken::{encode, EncodingKey, Header};

#[derive(Debug, Serialize, Deserialize)]
struct Room {
    id: String,
    created_at: i64,
    players: Vec<String>,
    hashed_password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub room_id: String,
    pub player_id: String,
    exp: usize,
}

type Rooms = Arc<Mutex<HashMap<String, Room>>>;

#[derive(Debug, Serialize, Deserialize)]
struct CreateRoomRequest {
    player_name: String,
    password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JoinRoomRequest {
    room_id: String,
    player_name: String,
    password: String,
}

#[derive(Deserialize)]
struct CleanupRequest {
    room_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RoomResponse {
    token: String,
    room_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListRoomsResponse {
    rooms: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RoomStatusResponse {
    room_id: String,
    players: Vec<String>,
    created_at: i64,
}

#[derive(Debug)]
enum ApiError {
    RoomNotFound,
    WrongPassword,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::RoomNotFound => {
                let body = Json(json!({"error": "Room not found"}));
                (StatusCode::NOT_FOUND, body).into_response()
            }
            ApiError::WrongPassword => {
                let body = Json(json!({"error": "Wrong password"}));
                (StatusCode::UNAUTHORIZED, body).into_response()
            }
        }
    }
}

pub const SECRET: &[u8] = b"super-secret-key";
const ROOM_EXPIRY_SECONDS: i64 = 600;

fn hash_password(password: &str) -> String {
    let salt: [u8; 16] = rand::random();
    let config = Config::default();

    argon2::hash_encoded(password.as_bytes(), &salt, &config).unwrap()
}

fn verify_password(stored_hash: &str, entered_password: &str) -> bool {
    argon2::verify_encoded(stored_hash, entered_password.as_bytes()).unwrap_or(false)
}

async fn create_room(State(rooms): State<Rooms>, Json(payload): Json<CreateRoomRequest>) -> Json<RoomResponse> {
    let room_id = Uuid::new_v4().to_string();
    let player_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    let room = Room {
        id: room_id.clone(),
        created_at: now,
        players: vec![player_id.clone()],
        hashed_password: hash_password(&payload.password),
    };

    rooms.lock().unwrap().insert(room_id.clone(), room);

    println!("room created: {} by player {}", room_id, payload.player_name);

    let claims = Claims {
        room_id: room_id.clone(),
        player_id,
        exp: (Utc::now() + Duration::seconds(ROOM_EXPIRY_SECONDS)).timestamp() as usize,
    };

    let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(SECRET)).unwrap();

    Json(RoomResponse { token, room_id })
}

async fn join_room(State(rooms): State<Rooms>, Json(payload): Json<JoinRoomRequest>) -> Result<Json<RoomResponse>, ApiError> {
    let mut rooms = rooms.lock().unwrap();
    let player_id = Uuid::new_v4().to_string();

    if let Some(room) = rooms.get_mut(&payload.room_id) {
        if !verify_password(&room.hashed_password, &payload.password) {
            return Err(ApiError::WrongPassword);
        }

        room.players.push(player_id.clone());

        println!("player {} joined Room {}", payload.player_name, payload.room_id);

        let claims = Claims {
            room_id: payload.room_id.clone(),
            player_id,
            exp: (Utc::now() + Duration::seconds(ROOM_EXPIRY_SECONDS)).timestamp() as usize,
        };

        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(SECRET)).unwrap();

        Ok(Json(RoomResponse {
            token,
            room_id: payload.room_id.clone(),
        }))
    } else {
        Err(ApiError::RoomNotFound)
    }
}

async fn list_rooms(State(rooms): State<Rooms>) -> Json<ListRoomsResponse> {
    let rooms = rooms.lock().unwrap();
    let room_ids = rooms.keys().cloned().collect();
    Json(ListRoomsResponse { rooms: room_ids })
}

async fn room_status(State(rooms): State<Rooms>, axum::extract::Path(room_id): axum::extract::Path<String>) -> Json<RoomStatusResponse> {
    let rooms = rooms.lock().unwrap();
    if let Some(room) = rooms.get(&room_id) {
        Json(RoomStatusResponse {
            room_id: room.id.clone(),
            players: room.players.clone(),
            created_at: room.created_at,
        })
    } else {
        panic!("room not found");
    }
}

async fn cleanup_room(State(rooms): State<Rooms>, Json(payload): Json<CleanupRequest>) -> StatusCode {
    let mut rooms = rooms.lock().unwrap();
    rooms.remove(&payload.room_id);
    StatusCode::OK
}

pub async fn run_main_server() {
    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route("/create_room", post(create_room))
        .route("/join_room", post(join_room))
        .route("/cleanup_room", post(cleanup_room))
        .route("/list_rooms", get(list_rooms))
        .route("/room_status/{room_id}", get(room_status))
        .with_state(rooms);

    println!("main server listening on 0.0.0.0:8080");
    let addr = SocketAddr::from(([0, 0, 0, 0], 8000));
    axum_server::bind(addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}