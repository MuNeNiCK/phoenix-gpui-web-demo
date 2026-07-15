use base64::Engine;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{FutureExt, StreamExt, select};
use gpui::{AsyncApp, Context, EventEmitter, Hsla, SharedString, Task, WeakEntity, rgb};
use phoenix_channel_client::{
    Channel, ChannelEvent, Options as ChannelOptions, Socket, SocketEvent, static_join_payload,
};
use phoenix_channel_runtime::{Payload, ProtocolEvent};
use phoenix_channel_runtime_web::{WebConnector, WebTimer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ops::Range;
use std::time::Duration;
use yrs::sync::{Awareness, DefaultProtocol, Message, Protocol, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{
    Assoc, Doc, GetString, IndexedSequence, OffsetKind, Options, ReadTxn, StickyIndex,
    Text as YText, TextRef, Transact,
};

use crate::documents::WorkspaceDocument;
use crate::text_offsets::{byte_offset_to_utf16, contiguous_diff, utf16_offset_to_byte};

#[derive(Clone)]
pub(crate) enum ConnectionState {
    Connecting,
    Online,
    Reconnecting,
    Error(SharedString),
}

pub(crate) enum CollaborationEvent {
    TextChanged(String),
    PresenceChanged,
    ConnectionChanged,
    DocumentsChanged(Vec<WorkspaceDocument>),
}

enum ClientCommand {
    Sync(Vec<u8>),
    Awareness(Vec<u8>),
    DeleteDocument(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AwarenessUser {
    pub(crate) name: String,
    pub(crate) color: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AwarenessCursor {
    anchor: StickyIndex,
    head: StickyIndex,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AwarenessState {
    user: AwarenessUser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<AwarenessCursor>,
}

pub(crate) struct RemoteCursor {
    pub(crate) user: AwarenessUser,
    pub(crate) offset: usize,
}

pub(crate) struct CollaborationSession {
    awareness: Awareness,
    local_user: AwarenessUser,
    local_cursor: Option<Range<usize>>,
    text: TextRef,
    connection: ConnectionState,
    outbound: UnboundedSender<ClientCommand>,
    _socket_task: Task<()>,
}

impl EventEmitter<CollaborationEvent> for CollaborationSession {}

impl CollaborationSession {
    pub(crate) fn new(
        document_id: String,
        documents: Vec<WorkspaceDocument>,
        cx: &mut Context<Self>,
    ) -> Self {
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let text = doc.get_or_insert_text("content");
        let awareness = Awareness::with_clock(doc, || js_sys::Date::now() as u64);
        let local_user = awareness_user(awareness.client_id());
        awareness
            .set_local_state(AwarenessState {
                user: local_user.clone(),
                cursor: None,
            })
            .expect("failed to initialize awareness state");
        let (outbound, receiver) = unbounded();
        let socket_task =
            Self::socket_task(cx, document_id, documents, awareness.client_id(), receiver);

        Self {
            awareness,
            local_user,
            local_cursor: None,
            text,
            connection: ConnectionState::Connecting,
            outbound,
            _socket_task: socket_task,
        }
    }

    pub(crate) fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    pub(crate) fn apply_local_text(&mut self, next: &str) {
        let current = self.text.get_string(&self.awareness.doc().transact());
        if current == next {
            return;
        }

        let (index, removed, inserted) = contiguous_diff(&current, next);
        let before = self.awareness.doc().transact().state_vector();
        {
            let mut txn = self.awareness.doc().transact_mut();
            if removed > 0 {
                self.text.remove_range(&mut txn, index, removed);
            }
            if !inserted.is_empty() {
                self.text.insert(&mut txn, index, &inserted);
            }
        }
        let update = self
            .awareness
            .doc()
            .transact()
            .encode_state_as_update_v1(&before);
        self.send_sync(Message::Sync(SyncMessage::Update(update)).encode_v1());
    }

    pub(crate) fn publish_local_cursor(&mut self, cursor: Option<Range<usize>>) {
        if self.local_cursor == cursor {
            return;
        }

        self.local_cursor = cursor.clone();
        let cursor = cursor.and_then(|range| {
            let value = self.text.get_string(&self.awareness.doc().transact());
            let anchor = byte_offset_to_utf16(&value, range.start);
            let head = byte_offset_to_utf16(&value, range.end);
            let txn = self.awareness.doc().transact();
            Some(AwarenessCursor {
                anchor: self
                    .text
                    .sticky_index(&txn, anchor, Assoc::After)
                    .unwrap_or_else(|| StickyIndex::from_type(&txn, &self.text, Assoc::After)),
                head: self
                    .text
                    .sticky_index(&txn, head, Assoc::After)
                    .unwrap_or_else(|| StickyIndex::from_type(&txn, &self.text, Assoc::After)),
            })
        });

        if self
            .awareness
            .set_local_state(AwarenessState {
                user: self.local_user.clone(),
                cursor,
            })
            .is_err()
        {
            return;
        }
        if let Ok(update) = self
            .awareness
            .update_with_clients([self.awareness.client_id()])
        {
            self.send_awareness(Message::Awareness(update).encode_v1());
        }
    }

    pub(crate) fn collaborators(&self) -> Vec<AwarenessUser> {
        let local_client_id = self.awareness.client_id();
        let mut users = vec![self.local_user.clone()];
        users.extend(self.awareness.iter().filter_map(|(client_id, state)| {
            if client_id == local_client_id {
                return None;
            }
            let state = state.data?;
            serde_json::from_str::<AwarenessState>(&state)
                .ok()
                .map(|state| state.user)
        }));
        users
    }

    pub(crate) fn remote_cursors(&self) -> Vec<RemoteCursor> {
        let local_client_id = self.awareness.client_id();
        let value = self.text.get_string(&self.awareness.doc().transact());
        let txn = self.awareness.doc().transact();
        self.awareness
            .iter()
            .filter_map(|(client_id, state)| {
                if client_id == local_client_id {
                    return None;
                }
                let state = state.data?;
                let state = serde_json::from_str::<AwarenessState>(&state).ok()?;
                let cursor = state.cursor?;
                let head = cursor.head.get_offset(&txn)?.index;
                Some(RemoteCursor {
                    user: state.user,
                    offset: utf16_offset_to_byte(&value, head),
                })
            })
            .collect()
    }

    pub(crate) fn delete_document(&self, document_id: String) {
        let _ = self
            .outbound
            .unbounded_send(ClientCommand::DeleteDocument(document_id));
    }

    fn clear_remote_awareness(&mut self) {
        let local_client_id = self.awareness.client_id();
        let remote_clients = self
            .awareness
            .iter()
            .map(|(client_id, _)| client_id)
            .filter(|client_id| *client_id != local_client_id)
            .collect::<Vec<_>>();
        for client_id in remote_clients {
            self.awareness.remove_state(client_id);
        }
    }

    fn initial_sync_message(&self) -> Vec<u8> {
        let state_vector = self.awareness.doc().transact().state_vector();
        Message::Sync(SyncMessage::SyncStep1(state_vector)).encode_v1()
    }

    fn initial_awareness_messages(&self) -> Vec<Vec<u8>> {
        let mut messages = vec![Message::AwarenessQuery.encode_v1()];
        if let Ok(update) = self.awareness.update() {
            messages.push(Message::Awareness(update).encode_v1());
        }
        messages
    }

    fn apply_remote_message(&mut self, message: &[u8], cx: &mut Context<Self>) -> Vec<Vec<u8>> {
        let responses = match DefaultProtocol.handle(&self.awareness, message) {
            Ok(responses) => responses,
            Err(error) => {
                self.set_connection(ConnectionState::Error(error.to_string().into()), cx);
                return Vec::new();
            }
        };
        let value = self.text.get_string(&self.awareness.doc().transact());
        cx.emit(CollaborationEvent::TextChanged(value));
        responses
            .into_iter()
            .map(|response| response.encode_v1())
            .collect()
    }

    fn apply_awareness_message(&mut self, message: &[u8], cx: &mut Context<Self>) -> Vec<Vec<u8>> {
        let responses = match DefaultProtocol.handle(&self.awareness, message) {
            Ok(responses) => responses,
            Err(error) => {
                self.set_connection(ConnectionState::Error(error.to_string().into()), cx);
                return Vec::new();
            }
        };
        cx.emit(CollaborationEvent::PresenceChanged);
        responses
            .into_iter()
            .map(|response| response.encode_v1())
            .collect()
    }

    fn remove_collaborator(&mut self, client_id: u64, cx: &mut Context<Self>) {
        if client_id != self.awareness.client_id() {
            self.awareness.remove_state(client_id);
            cx.emit(CollaborationEvent::PresenceChanged);
        }
    }

    fn set_connection(&mut self, state: ConnectionState, cx: &mut Context<Self>) {
        self.connection = state;
        cx.emit(CollaborationEvent::ConnectionChanged);
    }

    fn send_sync(&self, message: Vec<u8>) {
        let _ = self.outbound.unbounded_send(ClientCommand::Sync(message));
    }

    fn send_awareness(&self, message: Vec<u8>) {
        let _ = self
            .outbound
            .unbounded_send(ClientCommand::Awareness(message));
    }

    fn socket_task(
        cx: &mut Context<Self>,
        document_id: String,
        documents: Vec<WorkspaceDocument>,
        client_id: u64,
        mut receiver: UnboundedReceiver<ClientCommand>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let url = match websocket_url() {
                Ok(url) => url,
                Err(error) => {
                    set_connection_state(&this, cx, ConnectionState::Error(error.into()));
                    return;
                }
            };
            let options = ChannelOptions::default()
                .heartbeat_interval(Duration::from_secs(25))
                .request_timeout(Duration::from_secs(30));
            let (socket, driver) = Socket::new(WebConnector::new(url), WebTimer, options);
            wasm_bindgen_futures::spawn_local(driver);
            let topic = format!("documents:{document_id}");
            let mut channel = match socket.channel(
                &topic,
                static_join_payload(json!({
                    "client_id": client_id.to_string(),
                    "documents": documents,
                })),
            ) {
                Ok(channel) => channel,
                Err(error) => {
                    set_connection_state(
                        &this,
                        cx,
                        ConnectionState::Error(error.to_string().into()),
                    );
                    return;
                }
            };
            if let Err(error) = channel.join().await {
                set_connection_state(&this, cx, ConnectionState::Error(error.to_string().into()));
                return;
            }
            if !matches!(
                channel.next_event().await,
                Some(ChannelEvent::Protocol(ProtocolEvent::Joined { .. }))
            ) {
                set_connection_state(
                    &this,
                    cx,
                    ConnectionState::Error("channel closed during join".into()),
                );
                return;
            }
            set_connection_state(&this, cx, ConnectionState::Online);
            send_initial_messages(&this, cx, &channel).await;
            let mut socket_events = match socket.events() {
                Ok(events) => events,
                Err(error) => {
                    set_connection_state(
                        &this,
                        cx,
                        ConnectionState::Error(error.to_string().into()),
                    );
                    return;
                }
            };

            loop {
                enum Action {
                    Channel(Option<ChannelEvent>),
                    Socket(Option<SocketEvent>),
                    Command(Option<ClientCommand>),
                }
                let action = {
                    let channel_event = channel.next_event().fuse();
                    let socket_event = socket_events.next().fuse();
                    let command = receiver.next().fuse();
                    futures::pin_mut!(channel_event, socket_event, command);
                    select! {
                        event = channel_event => Action::Channel(event),
                        event = socket_event => Action::Socket(event),
                        command = command => Action::Command(command),
                    }
                };
                match action {
                    Action::Channel(Some(event)) => {
                        handle_channel_event(&this, cx, &channel, event).await
                    }
                    Action::Socket(Some(event)) => handle_socket_event(&this, cx, event),
                    Action::Command(Some(ClientCommand::Sync(message))) => {
                        if channel.cast("yjs", sync_payload(message)).await.is_err() {
                            set_connection_state(&this, cx, ConnectionState::Reconnecting);
                        }
                    }
                    Action::Command(Some(ClientCommand::Awareness(message))) => {
                        if channel
                            .cast("awareness", sync_payload(message))
                            .await
                            .is_err()
                        {
                            set_connection_state(&this, cx, ConnectionState::Reconnecting);
                        }
                    }
                    Action::Command(Some(ClientCommand::DeleteDocument(document_id))) => {
                        if channel
                            .cast("delete_document", json!({"document_id": document_id}))
                            .await
                            .is_err()
                        {
                            set_connection_state(&this, cx, ConnectionState::Reconnecting);
                        }
                    }
                    Action::Channel(None) | Action::Socket(None) | Action::Command(None) => return,
                }
            }
        })
    }
}

pub(crate) fn awareness_color(color: &str) -> Hsla {
    let color = color.trim_start_matches('#');
    u32::from_str_radix(color, 16)
        .map(|color| Hsla::from(rgb(color)))
        .unwrap_or_else(|_| Hsla::from(rgb(0x3b82f6)))
}

fn awareness_user(client_id: u64) -> AwarenessUser {
    const COLORS: [&str; 8] = [
        "#ef4444", "#f97316", "#eab308", "#22c55e", "#06b6d4", "#3b82f6", "#8b5cf6", "#ec4899",
    ];
    const NAMES: [&str; 8] = [
        "Amber Fox",
        "Coral Cat",
        "Golden Owl",
        "Mint Hare",
        "Cyan Jay",
        "Blue Lynx",
        "Violet Wolf",
        "Rose Swan",
    ];
    let index = client_id as usize % COLORS.len();
    AwarenessUser {
        name: format!("{} {:02X}", NAMES[index], (client_id >> 8) & 0xff),
        color: COLORS[index].to_string(),
    }
}

fn websocket_url() -> Result<String, String> {
    let location = web_sys::window()
        .ok_or_else(|| "browser window unavailable".to_string())?
        .location();
    let scheme = match location.protocol().map_err(js_error)?.as_str() {
        "https:" => "wss",
        _ => "ws",
    };
    let host = if location.port().map_err(js_error)? == "8080" {
        format!("{}:4000", location.hostname().map_err(js_error)?)
    } else {
        location.host().map_err(js_error)?
    };
    Ok(format!("{scheme}://{host}/socket/websocket?vsn=2.0.0"))
}

fn js_error(error: wasm_bindgen::JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "browser API error".to_string())
}

fn set_connection_state(
    this: &WeakEntity<CollaborationSession>,
    cx: &mut AsyncApp,
    state: ConnectionState,
) {
    this.update(cx, |this, cx| this.set_connection(state, cx))
        .ok();
}

fn sync_payload(message: Vec<u8>) -> serde_json::Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(message);
    json!({"message": encoded})
}

async fn send_initial_messages(
    this: &WeakEntity<CollaborationSession>,
    cx: &mut AsyncApp,
    channel: &Channel,
) {
    let sync = this.update(cx, |this, _| this.initial_sync_message()).ok();
    if let Some(message) = sync
        && channel.cast("yjs", sync_payload(message)).await.is_err()
    {
        set_connection_state(this, cx, ConnectionState::Reconnecting);
    }
    let messages = this
        .update(cx, |this, _| this.initial_awareness_messages())
        .unwrap_or_default();
    for message in messages {
        if channel
            .cast("awareness", sync_payload(message))
            .await
            .is_err()
        {
            set_connection_state(this, cx, ConnectionState::Reconnecting);
            break;
        }
    }
}

async fn handle_channel_event(
    this: &WeakEntity<CollaborationSession>,
    cx: &mut AsyncApp,
    channel: &Channel,
    event: ChannelEvent,
) {
    match event {
        ChannelEvent::Protocol(ProtocolEvent::Joined { .. }) => {
            set_connection_state(this, cx, ConnectionState::Online);
            send_initial_messages(this, cx, channel).await;
        }
        ChannelEvent::Protocol(ProtocolEvent::Message(frame))
            if frame.event == "yjs" || frame.event == "awareness" =>
        {
            let Some(encoded) = frame
                .payload
                .as_json()
                .and_then(|payload| payload.get("message"))
                .and_then(serde_json::Value::as_str)
            else {
                return;
            };
            let Ok(message) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                return;
            };
            let is_awareness = frame.event == "awareness";
            let responses = this
                .update(cx, |this, cx| {
                    if is_awareness {
                        this.apply_awareness_message(&message, cx)
                    } else {
                        this.apply_remote_message(&message, cx)
                    }
                })
                .unwrap_or_default();
            let event = if is_awareness { "awareness" } else { "yjs" };
            for response in responses {
                if channel.cast(event, sync_payload(response)).await.is_err() {
                    set_connection_state(this, cx, ConnectionState::Reconnecting);
                    break;
                }
            }
        }
        ChannelEvent::Protocol(ProtocolEvent::Message(frame)) if frame.event == "documents" => {
            let Some(documents) = frame
                .payload
                .as_json()
                .and_then(|payload| payload.get("documents"))
                .cloned()
                .and_then(|value| serde_json::from_value::<Vec<WorkspaceDocument>>(value).ok())
            else {
                return;
            };
            let _ = this.update(cx, |_, cx| {
                cx.emit(CollaborationEvent::DocumentsChanged(documents))
            });
        }
        ChannelEvent::Protocol(ProtocolEvent::Message(frame))
            if frame.event == "awareness_leave" =>
        {
            let Some(client_id) = frame
                .payload
                .as_json()
                .and_then(|payload| payload.get("client_id"))
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<u64>().ok())
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.remove_collaborator(client_id, cx));
        }
        ChannelEvent::Protocol(ProtocolEvent::JoinError { response, .. }) => {
            set_connection_state(
                this,
                cx,
                ConnectionState::Error(
                    format!("channel join rejected: {}", payload_message(&response)).into(),
                ),
            );
        }
        ChannelEvent::Protocol(
            ProtocolEvent::ChannelError { .. } | ProtocolEvent::RequestInterrupted { .. },
        )
        | ChannelEvent::Disconnected => {
            set_connection_state(this, cx, ConnectionState::Reconnecting)
        }
        ChannelEvent::Protocol(ProtocolEvent::ChannelClosed { .. }) => {
            set_connection_state(this, cx, ConnectionState::Error("channel closed".into()));
        }
        ChannelEvent::JoinPayloadError(error) => {
            set_connection_state(this, cx, ConnectionState::Error(error.into()));
        }
        ChannelEvent::Lagged { .. } => {
            set_connection_state(this, cx, ConnectionState::Reconnecting);
            send_initial_messages(this, cx, channel).await;
            set_connection_state(this, cx, ConnectionState::Online);
        }
        ChannelEvent::Protocol(_) => {}
    }
}

fn payload_message(payload: &Payload) -> String {
    match payload {
        Payload::Json(value) => value.to_string(),
        Payload::Binary(bytes) => format!("binary response ({} bytes)", bytes.len()),
        Payload::Reply { status, response } => format!("{status}: {}", payload_message(response)),
    }
}

fn handle_socket_event(
    this: &WeakEntity<CollaborationSession>,
    cx: &mut AsyncApp,
    event: SocketEvent,
) {
    match event {
        SocketEvent::Connecting { attempt: 0 } => {
            set_connection_state(this, cx, ConnectionState::Connecting)
        }
        SocketEvent::Connecting { .. }
        | SocketEvent::Disconnected { .. }
        | SocketEvent::ReconnectScheduled { .. } => {
            let _ = this.update(cx, |this, cx| {
                this.clear_remote_awareness();
                cx.emit(CollaborationEvent::PresenceChanged);
            });
            set_connection_state(this, cx, ConnectionState::Reconnecting);
        }
        SocketEvent::ReconnectStopped { reason, .. } => {
            set_connection_state(
                this,
                cx,
                ConnectionState::Error(format!("reconnect stopped: {reason}").into()),
            );
        }
        SocketEvent::Closed => set_connection_state(
            this,
            cx,
            ConnectionState::Error("channel client stopped".into()),
        ),
        SocketEvent::Connected | SocketEvent::Lagged { .. } => {}
    }
}
