use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    }, http::StatusCode, response::IntoResponse, routing::get, Router
};
use axum_extra::TypedHeader;
use serde::Deserialize;

use std::borrow::Cow;
use std::ops::ControlFlow;
use std::{net::SocketAddr};
use tower_http::{
    trace::{DefaultMakeSpan, TraceLayer},
};

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

//allows to extract the IP of connecting user
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::CloseFrame;

//allows to split the websocket stream into separate TX and RX branches
use futures::{sink::SinkExt, stream::StreamExt, TryStreamExt};

#[derive(Deserialize, Debug)]
struct LivekitRoom {
    room_id: String,
}


#[derive(Clone, Default)]
pub struct AppState {
  user_transcript: Option<async_channel::Receiver<String>>,
  backend_messages: Option<async_channel::Receiver<String>>,
}


#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ws_server=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let state = AppState::default();

    let app = Router::new()
        .route("/", get(ws_handler))
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    
    tracing::debug!("listening on {}", listener.local_addr().unwrap());

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

/// The handler for the HTTP request (this gets called when the HTTP GET lands at the start
/// of websocket negotiation). After this completes, the actual switching from HTTP to
/// websocket protocol will occur.
/// This is the last point where we can extract TCP/IP metadata such as IP address of the client
/// as well as things from HTTP headers such as user-agent of the browser etc.
async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    room_details: Option<Query<LivekitRoom>>,
    State(mut state): State<AppState>
) -> impl IntoResponse {
    let _room_id = match room_details{
        Some(e) => e.room_id.clone(),
        None => return (StatusCode::BAD_REQUEST, "livekit room id not passed as a query parameter").into_response(),
    };

    let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
        user_agent.to_string()
    } else {
        String::from("Unknown browser")
    };
    println!("`{user_agent}` at {addr} connected.");

    let (tx1, rx1) = async_channel::unbounded::<String>();
    let (tx2, rx2) = async_channel::unbounded::<String>();

    state.backend_messages = Some(rx1);
    state.user_transcript = Some(rx2);

    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket(socket, addr, state))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(mut socket: WebSocket, who: SocketAddr, state: AppState) {
    // send a ping (unsupported by some browsers) just to kick things off and get a response
    if socket.send(Message::Ping(vec![1, 2, 3])).await.is_ok() {
        println!("Pinged {who}...");
    } else {
        println!("Could not send ping {who}!");
        // no Error here since the only thing we can do is to close the connection.
        // If we can not send messages, there is no way to salvage the statemachine anyway.
        return;
    }

    // receive single message from a client (we can either receive or send with socket).
    // this will likely be the Pong for our Ping or a hello message from client.
    // waiting for message from a client will block this task, but will not block other client's
    // connections.

    if let Some(msg) = socket.recv().await {
        if let Ok(msg) = msg {
            if process_message(msg, who).is_break() {
                return;
            }
        } else {
            println!("client {who} abruptly disconnected");
            return;
        }
    }

    let (mut sender, mut receiver) = socket.split();

    let backend_msgs = match state.backend_messages{
        Some(e) => e,
        None => panic!("testing testing "),
    };
    
    let user_transcript = match state.user_transcript {
        Some(e) => e,
        None => panic!("testing testing "),
    };

    loop {
        while let Ok(e) = backend_msgs.try_recv(){
            sender.send(Message::Text(format!("Server message | hello ..."))).await;
        }
        
        while let Ok(e) = user_transcript.try_recv(){
            sender.send(Message::Text(format!("Server message | hello ..."))).await;
        }
        
        while let Ok(Some(e)) = receiver.try_next().await {
            sender.send(Message::Text(format!("Server message | {e:?} ..."))).await;
        }
    }

    // returning from the handler closes the websocket connection
    println!("Websocket context {who} destroyed");
}

/// helper to print contents of messages to stdout. Has special treatment for Close.
fn process_message(msg: Message, who: SocketAddr) -> ControlFlow<(), ()> {
    match msg {
        Message::Text(t) => {
            println!(">>> {who} sent str: {t:?}");
        }
        Message::Binary(d) => {
            println!(">>> {} sent {} bytes: {:?}", who, d.len(), d);
        }
        Message::Close(c) => {
            if let Some(cf) = c {
                println!(
                    ">>> {} sent close with code {} and reason `{}`",
                    who, cf.code, cf.reason
                );
            } else {
                println!(">>> {who} somehow sent close message without CloseFrame");
            }
            return ControlFlow::Break(());
        }

        Message::Pong(v) => {
            println!(">>> {who} sent pong with {v:?}");
        }
        // You should never need to manually handle Message::Ping, as axum's websocket library
        // will do so for you automagically by replying with Pong and copying the v according to
        // spec. But if you need the contents of the pings you can see them here.
        Message::Ping(v) => {
            println!(">>> {who} sent ping with {v:?}");
        }
    }
    ControlFlow::Continue(())
}
