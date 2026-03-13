use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use common::{ClientMsg, LastMessage, ServerMsg};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tower_http::services::ServeDir;
use tracing::{info, warn};

// MQTT topics:
pub const TOPIC_SEND: &str = "egui-axum-mqtt-demo/send"; // "Send" button publishes here
pub const TOPIC_POLL: &str = "egui-axum-mqtt-demo/poll"; // "Fetch" button reads last msg from here
pub const TOPIC_LIVE: &str = "egui-axum-mqtt-demo/live"; // Backend pushes live updates from here
pub const TOPIC_PING_REQ: &str = "egui-axum-mqtt-demo/ping/request";
pub const TOPIC_PING_RESP: &str = "egui-axum-mqtt-demo/ping/response";

#[derive(Clone)]
pub struct AppState {
    pub mqtt_client: AsyncClient,
    pub last_poll_msg: Arc<RwLock<Option<LastMessage>>>,
    /// Broadcast channel: backend + all WebSocket clients
    pub tx: broadcast::Sender<ServerMsg>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PingPayload {
    pub correlation_id: String,
    pub message: String,
}

/// Create MQTT client and event loop, and subscribe to relevant topics
pub async fn create_mqtt(client_id: &str, host: &str, port: u16) -> (AsyncClient, EventLoop) {
    let mut opts = MqttOptions::new(client_id, host, port);
    opts.set_keep_alive(std::time::Duration::from_secs(30));

    let (client, eventloop) = AsyncClient::new(opts, 50);

    client
        .subscribe(TOPIC_POLL, QoS::AtLeastOnce)
        .await
        .unwrap();
    client
        .subscribe(TOPIC_LIVE, QoS::AtLeastOnce)
        .await
        .unwrap();
    client
        .subscribe(TOPIC_PING_RESP, QoS::AtLeastOnce)
        .await
        .unwrap();

    (client, eventloop)
}

/// Build the shared application state
pub fn create_state(mqtt_client: AsyncClient) -> AppState {
    let (tx, _rx) = broadcast::channel::<ServerMsg>(100);

    AppState {
        mqtt_client,
        last_poll_msg: Arc::new(RwLock::new(None)),
        tx,
    }
}

/// Spawn the MQTT + AppState bridging loop, returning the JoinHandle
pub fn spawn_mqtt_loop(mut eventloop: EventLoop, state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let topic = publish.topic.clone();
                    let payload = String::from_utf8_lossy(&publish.payload).to_string();
                    info!("MQTT recv: {} -> {}", topic, payload);

                    match topic.as_str() {
                        TOPIC_POLL => {
                            let msg = LastMessage {
                                topic: topic.clone(),
                                payload: payload.clone(),
                                timestamp_ms: now_ms(),
                            };
                            *state.last_poll_msg.write().unwrap() = Some(msg);
                        }
                        TOPIC_LIVE => {
                            let _ = state.tx.send(ServerMsg::MqttUpdate { topic, payload });
                        }
                        TOPIC_PING_RESP => {
                            // Correlation ID in payload
                            if let Ok(resp) = serde_json::from_str::<PingPayload>(&payload) {
                                let _ = state.tx.send(ServerMsg::PingResponse {
                                    correlation_id: resp.correlation_id,
                                    device_reply: resp.message,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Ok(_) => {} // connack, suback, etc.
                Err(e) => {
                    warn!("MQTT error: {e:?}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    })
}

/// Build the Axum router (without fallback, for testing)
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/ws", get(ws_handler))
        .route("/api/last-message", get(get_last_message))
        .with_state(state)
}

/// Build the full router including static file serving
pub fn build_app(state: AppState) -> Router {
    // Serve the frontend Wasm app from ./dist (trunk output)
    build_router(state).fallback_service(ServeDir::new("dist"))
}

// HTTP handler: user-initiated poll
async fn get_last_message(State(state): State<AppState>) -> impl IntoResponse {
    let msg = state.last_poll_msg.read().unwrap().clone();
    match msg {
        Some(m) => Json(m).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "No messages yet").into_response(),
    }
}

// WebSocket handler
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();

    loop {
        tokio::select! {
            // Messages from the frontend:
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMsg>(&text) {
                            Ok(ClientMsg::Publish { payload }) => {
                                info!("Publishing to MQTT: {payload}");
                                let _ = state.mqtt_client
                                    .publish(TOPIC_SEND, QoS::AtLeastOnce, false, payload.as_bytes())
                                    .await;
                            }
                            Ok(ClientMsg::PingDevice { correlation_id }) => {
                                let ping = serde_json::to_string(&PingPayload {
                                    correlation_id,
                                    message: "ping".into(),
                                }).unwrap();
                                let _ = state.mqtt_client
                                    .publish(TOPIC_PING_REQ, QoS::AtLeastOnce, false, ping.as_bytes())
                                    .await;
                            }
                            Err(e) => warn!("Bad client message: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // Messages from MQTT → push to frontend:
            Ok(server_msg) = rx.recv() => {
                let json = serde_json::to_string(&server_msg).unwrap();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break; // client disconnected
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
