//! `ActionCable::Connection::Base` and `Connection::Subscriptions`: one task per socket.
//!
//! Commands are handled one at a time in arrival order. Stream deliveries arrive from per-stream
//! forwarder tasks through a bounded queue, so a client that stops reading eventually makes its
//! streams lag, which closes the connection with `reconnect: true`.
use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::channel::{Channel, Params, Subscription};
use crate::protocol::{self, DisconnectReason};
use crate::pubsub::RecvError;
use crate::server::{ConnectRequest, Identified, internal_channel};
use crate::{Server, json};

pub(crate) enum Control {
    /// A stream subscriber fell behind.
    Lagged,
    /// A message on this connection's internal channel (`action_cable/<identifier>`).
    Internal(Arc<str>),
}

struct Entry<U: Send + Sync + 'static> {
    channel: Box<dyn Channel<U>>,
    sub: Subscription<U>,
}

/// Why the socket is going away, once we've decided to close it.
struct Close {
    reason: Option<DisconnectReason>,
    reconnect: Value,
}

struct Connection<U: Send + Sync + 'static> {
    server: Server<U>,
    user: Arc<U>,
    /// Keyed by the raw identifier string, in subscription order (a Ruby hash).
    subscriptions: Vec<(String, Entry<U>)>,
    pending: Vec<String>,
    outbound: mpsc::Sender<String>,
    control: mpsc::Sender<Control>,
}

pub(crate) async fn run<U: Identified + Send + Sync + 'static>(server: Server<U>, socket: WebSocket, request: ConnectRequest) {
    let (mut sink, mut stream) = socket.split();
    let config = server.config().clone();

    // handle_open: connect, subscribe to the internal channel, welcome, then process whatever
    // arrived meanwhile (the socket buffers it for us, like MessageBuffer).
    let Some(user) = server.authenticator().connect(&request).await else {
        tracing::error!("An unauthorized connection attempt was rejected");
        let frame = protocol::disconnect(Some(DisconnectReason::Unauthorized), &Value::Bool(false));
        let _ = sink.send(Message::Text(frame.into())).await;
        close_socket(&mut sink, &mut stream, config.close_timeout).await;
        return;
    };

    let (outbound, mut outbound_rx) = mpsc::channel::<String>(config.outbound_capacity);
    let (control, mut control_rx) = mpsc::channel::<Control>(16);
    let identifier = user.connection_identifier();
    let internal = (!identifier.is_empty()).then(|| spawn_internal_subscriber(&server, &identifier, control.clone()));

    let mut heartbeat = server.heartbeat();
    heartbeat.mark_unchanged();
    let mut restarts = server.restarts();

    let mut connection =
        Connection { server, user: Arc::new(user), subscriptions: Vec::new(), pending: Vec::new(), outbound, control };

    let mut close: Option<Close> = None;
    if sink.send(Message::Text(protocol::welcome().into())).await.is_err() {
        connection.handle_close().await;
        return;
    }

    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => connection.dispatch(text.as_str()).await,
                Some(Ok(Message::Binary(_))) => tracing::error!("Couldn't handle non-string message: Array"),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
            Some(frame) = outbound_rx.recv() => connection.pending.push(frame),
            Some(control) = control_rx.recv() => {
                close = Some(match control {
                    Control::Lagged => Close { reason: None, reconnect: Value::Bool(true) },
                    Control::Internal(message) => match process_internal_message(&message) {
                        Some(close) => close,
                        None => continue,
                    },
                });
            }
            Ok(()) = heartbeat.changed() => {
                let now = *heartbeat.borrow_and_update();
                connection.pending.push(protocol::ping(now));
            }
            Ok(()) = restarts.recv() => {
                close = Some(Close { reason: Some(DisconnectReason::ServerRestart), reconnect: Value::Bool(true) });
            }
        }

        if !connection.flush(&mut sink).await {
            break;
        }
        if let Some(Close { reason, reconnect }) = close.take() {
            let _ = sink.send(Message::Text(protocol::disconnect(reason, &reconnect).into())).await;
            close_socket(&mut sink, &mut stream, config.close_timeout).await;
            break;
        }
    }

    if let Some(internal) = internal {
        internal.abort();
    }
    connection.handle_close().await;
}

/// `InternalChannel#process_internal_message`.
fn process_internal_message(message: &str) -> Option<Close> {
    let message: Value = serde_json::from_str(message).ok()?;
    (message.get("type")? == "disconnect").then(|| Close {
        reason: Some(DisconnectReason::Remote),
        reconnect: message.get("reconnect").cloned().unwrap_or(Value::Bool(true)),
    })
}

fn spawn_internal_subscriber<U: Send + Sync + 'static>(
    server: &Server<U>,
    identifier: &str,
    control: mpsc::Sender<Control>,
) -> tokio::task::JoinHandle<()> {
    let mut subscriber = server.hub().subscribe(&internal_channel(identifier));
    tokio::spawn(async move {
        loop {
            match subscriber.recv().await {
                Ok(message) => {
                    if control.send(Control::Internal(message)).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// Sends a normal close (1000, no reason, as `ClientSocket#close` defaults) and waits briefly
/// for the client to finish the handshake.
async fn close_socket(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    timeout: std::time::Duration,
) {
    let frame = CloseFrame { code: 1000, reason: "".into() };
    if sink.send(Message::Close(Some(frame))).await.is_ok() {
        let _ = tokio::time::timeout(timeout, async {
            while let Some(Ok(message)) = stream.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        })
        .await;
    }
}

impl<U: Send + Sync + 'static> Connection<U> {
    async fn flush(&mut self, sink: &mut futures_util::stream::SplitSink<WebSocket, Message>) -> bool {
        for frame in self.pending.drain(..) {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                return false;
            }
        }
        true
    }

    /// `Subscriptions#execute_command`. Anything malformed raises in Rails, which is logged and
    /// otherwise ignored; the connection stays open.
    async fn dispatch(&mut self, text: &str) {
        let data = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(data)) => data,
            _ => return tracing::error!(message = text, "Could not execute command"),
        };
        match data.get("command").and_then(Value::as_str) {
            Some("subscribe") => self.add(&data).await,
            Some("unsubscribe") => self.remove(&data).await,
            Some("message") => self.perform_action(&data).await,
            _ => tracing::error!(message = text, "Received unrecognized command"),
        }
    }

    /// `Subscriptions#add`. A repeated identifier (byte for byte) is ignored without a reply.
    async fn add(&mut self, data: &Params) {
        let Some(identifier) = data.get("identifier").and_then(Value::as_str) else {
            return tracing::error!("Could not execute command: missing identifier");
        };
        let Ok(Value::Object(params)) = serde_json::from_str::<Value>(identifier) else {
            return tracing::error!(identifier, "Could not execute command: invalid identifier");
        };
        if self.position(identifier).is_some() {
            return;
        }
        let class_name = params.get("channel").and_then(Value::as_str).unwrap_or_default().to_string();
        let Some(factory) = self.server.channel_factory(&class_name) else {
            return tracing::error!(channel = class_name, "Subscription class not found");
        };

        let channel = factory();
        let sub = Subscription {
            server: self.server.clone(),
            class_name,
            identifier: identifier.to_string(),
            encoded_identifier: json::encode(identifier).into(),
            params,
            current_user: self.user.clone(),
            streams: Vec::new(),
            rejected: false,
            unsubscribed: false,
            transmissions: Vec::new(),
            outbound: self.outbound.clone(),
            control: self.control.clone(),
        };
        self.subscriptions.push((identifier.to_string(), Entry { channel, sub }));
        self.subscribe_to_channel(identifier).await;
    }

    /// `Channel::Base#subscribe_to_channel`.
    async fn subscribe_to_channel(&mut self, identifier: &str) {
        let index = self.position(identifier).expect("just added");
        let Entry { channel, sub } = &mut self.subscriptions[index].1;
        let result = channel.subscribed(sub).await;
        self.pending.append(&mut sub.transmissions);

        if let Err(error) = result {
            return tracing::error!(identifier, error = error.0, "Could not execute command");
        }
        if sub.rejected {
            self.remove_subscription(index).await;
            self.pending.push(protocol::rejection(identifier));
        } else {
            self.pending.push(protocol::confirmation(identifier));
        }
    }

    /// `Subscriptions#remove`: no reply either way.
    async fn remove(&mut self, data: &Params) {
        match self.find(data) {
            Some(index) => self.remove_subscription(index).await,
            None => tracing::error!("Unable to find subscription with identifier"),
        }
    }

    /// `Subscriptions#remove_subscription` → `Channel::Base#unsubscribe_from_channel`.
    async fn remove_subscription(&mut self, index: usize) {
        let (_, Entry { mut channel, mut sub }) = self.subscriptions.remove(index);
        sub.unsubscribed = true;
        if let Err(error) = channel.unsubscribed(&mut sub).await {
            tracing::error!(error = error.0, "Could not execute command");
        }
        sub.stop_all_streams();
        self.pending.append(&mut sub.transmissions);
    }

    /// `Subscriptions#perform_action` → `Channel::Base#perform_action`.
    async fn perform_action(&mut self, data: &Params) {
        let Some(index) = self.find(data) else {
            return tracing::error!("Unable to find subscription with identifier");
        };
        let payload = match data.get("data").and_then(Value::as_str).map(serde_json::from_str::<Value>) {
            Some(Ok(Value::Object(payload))) => payload,
            _ => return tracing::error!("Could not execute command: invalid data"),
        };
        let action = match payload.get("action") {
            None | Some(Value::Null) => "receive".to_string(),
            Some(Value::String(action)) if action.trim().is_empty() => "receive".to_string(),
            Some(Value::String(action)) => action.clone(),
            Some(_) => return tracing::error!("Could not execute command: invalid action"),
        };

        let Entry { channel, sub } = &mut self.subscriptions[index].1;
        let result = channel.perform(&action, &payload, sub).await;
        self.pending.append(&mut sub.transmissions);
        match result {
            Ok(true) => {}
            Ok(false) => tracing::error!(action, "Unable to process"),
            Err(error) => tracing::error!(action, error = error.0, "Could not execute command"),
        }
    }

    /// `Connection::Base#handle_close`: unsubscribe everything.
    async fn handle_close(&mut self) {
        while !self.subscriptions.is_empty() {
            self.remove_subscription(0).await;
        }
    }

    fn find(&self, data: &Params) -> Option<usize> {
        data.get("identifier").and_then(Value::as_str).and_then(|identifier| self.position(identifier))
    }

    fn position(&self, identifier: &str) -> Option<usize> {
        self.subscriptions.iter().position(|(id, _)| id == identifier)
    }
}
