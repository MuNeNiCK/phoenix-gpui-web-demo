use base64::Engine;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{FutureExt, SinkExt, StreamExt, select};
use gloo_net::websocket::{Message as WebSocketMessage, futures::WebSocket};
use gloo_timers::future::{IntervalStream, TimeoutFuture};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, FontWeight, SharedString, Task, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use guise::prelude::*;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;
use yrs::sync::{Awareness, DefaultProtocol, Message, Protocol, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, OffsetKind, Options, ReadTxn, Text as YText, TextRef, Transact};

const DOCUMENT_ID: &str = "demo";
const TOPIC: &str = "documents:demo";

thread_local! {
    // `Platform::run` returns immediately in the browser. Retain GPUI's app
    // handle so its windows, event listeners, and animation frame stay alive.
    static APPLICATION: RefCell<Option<gpui::ApplicationHandle>> = const { RefCell::new(None) };
}

fn boot_status() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .get_element_by_id("boot-status")
}

fn show_boot_error(message: &str) {
    if let Some(status) = boot_status() {
        status.set_class_name("error");
        status.set_text_content(Some(message));
    }
}

fn hide_boot_status() {
    if let Some(status) = boot_status() {
        status.remove();
    }
}

#[derive(Clone)]
enum ConnectionState {
    Connecting,
    Online,
    Reconnecting,
    Error(SharedString),
}

enum ClientCommand {
    Sync(Vec<u8>),
}

struct DemoApp {
    awareness: Awareness,
    text: TextRef,
    editor: Entity<TextArea>,
    connection: ConnectionState,
    outbound: UnboundedSender<ClientCommand>,
    _socket_task: Task<()>,
}

impl DemoApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let text = doc.get_or_insert_text("content");
        let awareness = Awareness::with_clock(doc, || js_sys::Date::now() as u64);
        let editor = cx.new(|cx| {
            TextArea::new(cx)
                .rows(16)
                .label("Shared document")
                .description("Open this page in another tab and edit from either window.")
                .placeholder("Start writing together...")
        });
        let (outbound, receiver) = unbounded();

        cx.subscribe(&editor, |this, _editor, event: &TextAreaEvent, cx| {
            this.apply_local_text(&event.0);
            cx.notify();
        })
        .detach();

        let socket_task = Self::socket_task(cx, receiver);

        Self {
            awareness,
            text,
            editor,
            connection: ConnectionState::Connecting,
            outbound,
            _socket_task: socket_task,
        }
    }

    fn apply_local_text(&mut self, next: &str) {
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

    fn initial_sync_message(&self) -> Vec<u8> {
        let state_vector = self.awareness.doc().transact().state_vector();
        Message::Sync(SyncMessage::SyncStep1(state_vector)).encode_v1()
    }

    fn apply_remote_message(&mut self, message: &[u8], cx: &mut Context<Self>) -> Vec<Vec<u8>> {
        let responses = match DefaultProtocol.handle(&self.awareness, message) {
            Ok(responses) => responses,
            Err(error) => {
                self.connection = ConnectionState::Error(error.to_string().into());
                cx.notify();
                return Vec::new();
            }
        };

        let value = self.text.get_string(&self.awareness.doc().transact());
        self.editor.update(cx, |editor, cx| {
            if editor.text() != value {
                editor.set_text(&value, cx);
            }
        });
        cx.notify();

        responses
            .into_iter()
            .map(|response| response.encode_v1())
            .collect()
    }

    fn send_sync(&self, message: Vec<u8>) {
        let _ = self.outbound.unbounded_send(ClientCommand::Sync(message));
    }

    fn socket_task(
        cx: &mut Context<Self>,
        mut receiver: UnboundedReceiver<ClientCommand>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                this.update(cx, |this, cx| {
                    this.connection = ConnectionState::Connecting;
                    cx.notify();
                })
                .ok();

                let url = match websocket_url() {
                    Ok(url) => url,
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.connection = ConnectionState::Error(error.into());
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                };

                let mut socket = match WebSocket::open(&url) {
                    Ok(socket) => socket,
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.connection = ConnectionState::Error(error.to_string().into());
                            cx.notify();
                        })
                        .ok();
                        TimeoutFuture::new(1_000).await;
                        continue;
                    }
                };

                let mut reference = 1_u64;
                let join_reference = reference.to_string();
                let join = phoenix_frame(
                    Some(&join_reference),
                    &reference.to_string(),
                    TOPIC,
                    "phx_join",
                    json!({}),
                );

                if socket.send(WebSocketMessage::Text(join)).await.is_err() {
                    TimeoutFuture::new(1_000).await;
                    continue;
                }

                let mut joined = false;
                let mut pending = VecDeque::new();
                let mut heartbeat = IntervalStream::new(25_000).fuse();

                loop {
                    let incoming = socket.next().fuse();
                    let command = receiver.next().fuse();
                    futures::pin_mut!(incoming, command);

                    select! {
                        frame = incoming => {
                            match frame {
                                Some(Ok(WebSocketMessage::Text(frame))) => {
                                    let Some((event, payload)) = parse_phoenix_frame(&frame) else {
                                        continue;
                                    };

                                    if event == "phx_reply"
                                        && payload.get("status").and_then(Value::as_str) == Some("ok")
                                        && !joined
                                    {
                                        joined = true;
                                        this.update(cx, |this, cx| {
                                            this.connection = ConnectionState::Online;
                                            cx.notify();
                                        }).ok();

                                        let initial = this.update(cx, |this, _cx| {
                                            this.initial_sync_message()
                                        }).ok();
                                        if let Some(initial) = initial {
                                            reference += 1;
                                            if send_sync_frame(
                                                &mut socket,
                                                &join_reference,
                                                reference,
                                                initial,
                                            ).await.is_err() {
                                                break;
                                            }
                                        }

                                        while let Some(message) = pending.pop_front() {
                                            reference += 1;
                                            if send_sync_frame(
                                                &mut socket,
                                                &join_reference,
                                                reference,
                                                message,
                                            ).await.is_err() {
                                                break;
                                            }
                                        }
                                    } else if event == "yjs" {
                                        let Some(encoded) = payload.get("message").and_then(Value::as_str) else {
                                            continue;
                                        };
                                        let Ok(message) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                                            continue;
                                        };

                                        let responses = this.update(cx, |this, cx| {
                                            this.apply_remote_message(&message, cx)
                                        }).unwrap_or_default();

                                        for response in responses {
                                            reference += 1;
                                            if send_sync_frame(
                                                &mut socket,
                                                &join_reference,
                                                reference,
                                                response,
                                            ).await.is_err() {
                                                break;
                                            }
                                        }
                                    } else if event == "phx_error" || event == "phx_close" {
                                        break;
                                    }
                                }
                                Some(Ok(WebSocketMessage::Bytes(_))) => {}
                                Some(Err(_)) | None => break,
                            }
                        },
                        command = command => {
                            let Some(ClientCommand::Sync(message)) = command else {
                                return;
                            };
                            if joined {
                                reference += 1;
                                if send_sync_frame(
                                    &mut socket,
                                    &join_reference,
                                    reference,
                                    message,
                                ).await.is_err() {
                                    break;
                                }
                            } else {
                                pending.push_back(message);
                            }
                        },
                        _ = heartbeat.select_next_some() => {
                            reference += 1;
                            let frame = phoenix_frame(
                                None,
                                &reference.to_string(),
                                "phoenix",
                                "heartbeat",
                                json!({}),
                            );
                            if socket.send(WebSocketMessage::Text(frame)).await.is_err() {
                                break;
                            }
                        },
                    }
                }

                this.update(cx, |this, cx| {
                    this.connection = ConnectionState::Reconnecting;
                    cx.notify();
                })
                .ok();
                TimeoutFuture::new(1_000).await;
            }
        })
    }
}

impl Render for DemoApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let body = theme.body().hsla();
        let text = theme.text().hsla();
        let dimmed = theme.dimmed().hsla();
        let border = theme.border().hsla();
        let font = theme.font_family.clone();
        let is_dark = theme.scheme.is_dark();

        let (badge, status) = match &self.connection {
            ConnectionState::Connecting => (
                Badge::new("connecting").color(ColorName::Yellow),
                SharedString::from("Connecting to the shared document..."),
            ),
            ConnectionState::Online => (
                Badge::new("live").color(ColorName::Teal),
                SharedString::from("Changes are synchronized through Phoenix."),
            ),
            ConnectionState::Reconnecting => (
                Badge::new("reconnecting").color(ColorName::Yellow),
                SharedString::from("Connection lost. Reconnecting automatically..."),
            ),
            ConnectionState::Error(error) => (
                Badge::new("error").color(ColorName::Red),
                format!("Synchronization error: {error}").into(),
            ),
        };

        let header = Group::new()
            .justify(Justify::Between)
            .child(
                Stack::new()
                    .gap(Size::Xs)
                    .child(Title::new("Collaborative Notes").order(1))
                    .child(Text::new("A CRDT editor rendered by GPUI Web").dimmed()),
            )
            .child(
                Group::new()
                    .gap(Size::Sm)
                    .child(Badge::new("WASM").color(ColorName::Grape))
                    .child(
                        Button::new("theme-toggle", if is_dark { "Light" } else { "Dark" })
                            .variant(Variant::Default)
                            .on_click(cx.listener(|_, _, window, cx| {
                                cx.global_mut::<Theme>().scheme =
                                    cx.global::<Theme>().scheme.toggled();
                                window.refresh();
                            })),
                    ),
            );

        let editor_card = Paper::new()
            .with_border(true)
            .shadow(Size::Sm)
            .padding(Size::Lg)
            .child(
                Stack::new()
                    .gap(Size::Md)
                    .child(
                        Group::new()
                            .justify(Justify::Between)
                            .child(Title::new(format!("Document: {DOCUMENT_ID}")).order(3))
                            .child(badge),
                    )
                    .child(Text::new(status).size(Size::Sm).dimmed())
                    .child(self.editor.clone()),
            );

        div()
            .id("app-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(body)
            .text_color(text)
            .font_family(font)
            .child(
                div()
                    .w_full()
                    .max_w(px(960.0))
                    .mx_auto()
                    .p(px(32.0))
                    .child(
                        Stack::new()
                            .gap(Size::Xl)
                            .child(header)
                            .child(
                                div()
                                    .border_1()
                                    .border_color(border)
                                    .rounded(px(12.0))
                                    .p(px(18.0))
                                    .child(
                                        Group::new()
                                            .gap(Size::Sm)
                                            .child(Badge::new("Architecture").color(ColorName::Blue))
                                            .child(
                                                div()
                                                    .text_color(dimmed)
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("GPUI Web + Yrs → Phoenix Channel → Yex/Rustler"),
                                            ),
                                    ),
                            )
                            .child(editor_card)
                            .child(
                                Text::new(
                                    "Each browser keeps a local Yrs document; Phoenix coordinates the shared Yex document.",
                                )
                                .size(Size::Sm)
                                .dimmed(),
                            ),
                    ),
            )
    }
}

fn contiguous_diff(current: &str, next: &str) -> (u32, u32, String) {
    let current_chars: Vec<char> = current.chars().collect();
    let next_chars: Vec<char> = next.chars().collect();
    let mut prefix = 0;

    while prefix < current_chars.len()
        && prefix < next_chars.len()
        && current_chars[prefix] == next_chars[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < current_chars.len() - prefix
        && suffix < next_chars.len() - prefix
        && current_chars[current_chars.len() - 1 - suffix]
            == next_chars[next_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let index = utf16_len(&current_chars[..prefix]);
    let removed = utf16_len(&current_chars[prefix..current_chars.len() - suffix]);
    let inserted = next_chars[prefix..next_chars.len() - suffix]
        .iter()
        .collect();
    (index, removed, inserted)
}

fn utf16_len(chars: &[char]) -> u32 {
    chars
        .iter()
        .map(|character| character.len_utf16() as u32)
        .sum()
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

fn phoenix_frame(
    join_reference: Option<&str>,
    reference: &str,
    topic: &str,
    event: &str,
    payload: Value,
) -> String {
    json!([join_reference, reference, topic, event, payload]).to_string()
}

fn parse_phoenix_frame(frame: &str) -> Option<(String, Value)> {
    let values: Vec<Value> = serde_json::from_str(frame).ok()?;
    if values.len() != 5 || values[2].as_str()? != TOPIC {
        return None;
    }
    Some((values[3].as_str()?.to_string(), values[4].clone()))
}

async fn send_sync_frame(
    socket: &mut WebSocket,
    join_reference: &str,
    reference: u64,
    message: Vec<u8>,
) -> Result<(), gloo_net::websocket::WebSocketError> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(message);
    let frame = phoenix_frame(
        Some(join_reference),
        &reference.to_string(),
        TOPIC,
        "yjs",
        json!({"message": encoded}),
    );
    socket.send(WebSocketMessage::Text(frame)).await
}

fn main() {
    gpui_platform::web_init();
    log::set_max_level(log::LevelFilter::Error);

    let application = gpui_platform::single_threaded_web().run_embedded(|cx: &mut App| {
        Theme::dark().init(cx);

        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(DemoApp::new),
        );

        match window {
            Ok(_) => {
                hide_boot_status();
                cx.activate(true);
            }
            Err(error) => {
                log::error!("failed to open GPUI Web window: {error:#}");
                show_boot_error(
                    "WebGPU adapter unavailable. Enable hardware acceleration. On Linux Chromium, restart with --enable-unsafe-webgpu.",
                );
            }
        }
    });

    APPLICATION.with(|slot| *slot.borrow_mut() = Some(application));
}

#[cfg(test)]
mod tests {
    use super::contiguous_diff;

    #[test]
    fn calculates_utf16_text_differences() {
        assert_eq!(contiguous_diff("hello", "help"), (3, 2, "p".to_string()));
        assert_eq!(contiguous_diff("a😀b", "a🌱b"), (1, 2, "🌱".to_string()));
        assert_eq!(contiguous_diff("same", "same"), (4, 0, String::new()));
    }
}
