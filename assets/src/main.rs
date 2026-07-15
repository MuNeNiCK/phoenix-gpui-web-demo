use base64::Engine;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{FutureExt, StreamExt, select};
use gpui::prelude::*;
use gpui::{
    App, AsyncApp, Bounds, Context, Entity, FontFallbacks, SharedString, Subscription, Task,
    WeakEntity, Window, WindowBounds, WindowOptions, px, size,
};
use gpui_component::{
    ActiveTheme, IconName, IndexPath, Root, Theme, ThemeMode, ThemeRegistry, WindowExt,
    breadcrumb::Breadcrumb,
    button::Button,
    form::{field, v_form},
    h_flex,
    input::{Input, InputEvent, InputState},
    select::{Select, SelectEvent, SelectState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_component_assets::Assets;
use phoenix_channel_client::{
    Channel, ChannelEvent, Options as ChannelOptions, Socket, SocketEvent, static_join_payload,
};
use phoenix_channel_runtime::{Payload, ProtocolEvent};
use phoenix_channel_runtime_web::{WebConnector, WebTimer};
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::time::Duration;
use yrs::sync::{Awareness, DefaultProtocol, Message, Protocol, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, OffsetKind, Options, ReadTxn, Text as YText, TextRef, Transact};

const DOCUMENT_ID: &str = "shared-notes";
const TOPIC: &str = "documents:shared-notes";
const JAPANESE_FONT_FAMILY: &str = "Noto Sans JP";
const EMBEDDED_THEMES: &[&str] = &[
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/tokyonight.json"),
];

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

struct WorkspaceApp {
    awareness: Awareness,
    text: TextRef,
    editor: Entity<InputState>,
    search: Entity<InputState>,
    theme_select: Entity<SelectState<Vec<SharedString>>>,
    connection: ConnectionState,
    outbound: UnboundedSender<ClientCommand>,
    _subscriptions: Vec<Subscription>,
    _socket_task: Task<()>,
}

impl WorkspaceApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let text = doc.get_or_insert_text("content");
        let awareness = Awareness::with_clock(doc, || js_sys::Date::now() as u64);
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .folding(false)
                .line_number(true)
                .soft_wrap(true)
                .scroll_beyond_last_line(Some(8))
                .placeholder("Start writing...")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search files"));
        let themes = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .map(|theme| theme.name.clone())
            .collect::<Vec<_>>();
        let active_theme = cx.theme().theme_name();
        let selected_theme = themes
            .iter()
            .position(|theme_name| theme_name == active_theme)
            .map(|index| IndexPath::default().row(index));
        let theme_select = cx.new(|cx| SelectState::new(themes, selected_theme, window, cx));
        let (outbound, receiver) = unbounded();

        let editor_subscription = cx.subscribe_in(&editor, window, {
            let editor = editor.clone();
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = editor.read(cx).value();
                    this.apply_local_text(value.as_ref());
                    cx.notify();
                }
            }
        });
        let search_subscription = cx.subscribe_in(&search, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let theme_subscription = cx.subscribe_in(
            &theme_select,
            window,
            |_, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(Some(theme_name)) = event else {
                    return;
                };
                select_theme(theme_name, window, cx);
            },
        );

        let socket_task = Self::socket_task(cx, receiver);

        Self {
            awareness,
            text,
            editor,
            search,
            theme_select,
            connection: ConnectionState::Connecting,
            outbound,
            _subscriptions: vec![editor_subscription, search_subscription, theme_subscription],
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

    fn apply_remote_message(
        &mut self,
        message: &[u8],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Vec<u8>> {
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
            if editor.value().as_ref() != value {
                editor.set_value(value, window, cx);
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

            let mut channel = match socket.channel(TOPIC, static_join_payload(json!({}))) {
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

            // `join` resolves before the corresponding event is consumed.
            // Consume that event once, then perform the initial CRDT handshake.
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
            send_initial_sync(&this, cx, &channel).await;

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
                        handle_channel_event(&this, cx, &channel, event).await;
                    }
                    Action::Socket(Some(event)) => {
                        handle_socket_event(&this, cx, event);
                    }
                    Action::Command(Some(ClientCommand::Sync(message))) => {
                        if channel.cast("yjs", sync_payload(message)).await.is_err() {
                            set_connection_state(&this, cx, ConnectionState::Reconnecting);
                        }
                    }
                    Action::Channel(None) | Action::Socket(None) | Action::Command(None) => return,
                }
            }
        })
    }
}

impl Render for WorkspaceApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let theme = cx.theme();
        let font = theme.font_family.clone();
        let mut default_font = gpui::font(font);
        default_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            JAPANESE_FONT_FAMILY.to_string(),
        ]));
        let value = self.editor.read(cx).value();
        let line_count = value.lines().count().max(1);

        let connection_detail = match &self.connection {
            ConnectionState::Connecting => SharedString::from("Opening Phoenix Channel"),
            ConnectionState::Online => SharedString::from("All changes synchronized"),
            ConnectionState::Reconnecting => SharedString::from("Waiting for the server"),
            ConnectionState::Error(error) => format!("Synchronization error: {error}").into(),
        };
        let theme_select = self.theme_select.clone();

        let search_query = self.search.read(cx).value();
        let query = search_query.trim().to_lowercase();
        let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
        let mut files = Vec::new();
        if matches("shared-notes.md") {
            files.push(
                SidebarMenuItem::new("shared-notes.md")
                    .icon(IconName::File)
                    .active(true),
            );
        }
        if matches("README.md") {
            files.push(SidebarMenuItem::new("README.md").icon(IconName::File));
        }
        if matches("lib") {
            files.push(SidebarMenuItem::new("lib").icon(IconName::Folder));
        }
        if matches("config") {
            files.push(SidebarMenuItem::new("config").icon(IconName::Folder));
        }
        if files.is_empty() {
            files.push(SidebarMenuItem::new("No matching files").disable(true));
        }

        let sidebar = Sidebar::new("workspace-sidebar")
            .header(Input::new(&self.search))
            .child(SidebarGroup::new("Workspace").child(SidebarMenu::new().children(files)))
            .footer(
                Button::new("settings")
                    .icon(IconName::Settings2)
                    .label("Settings")
                    .on_click(move |_, window, cx| {
                        let theme_select = theme_select.clone();
                        window.open_dialog(cx, move |dialog, _, _| {
                            dialog.title("Settings").child(
                                v_form().child(
                                    field()
                                        .label("Theme")
                                        .description("Choose the application color theme.")
                                        .child(Select::new(&theme_select)),
                                ),
                            )
                        });
                    }),
            );

        let editor = v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                TabBar::new("editor-tabs")
                    .selected_index(0)
                    .child(Tab::new().label("shared-notes.md")),
            )
            .child(
                h_flex().min_w_0().overflow_hidden().p_2().child(
                    Breadcrumb::new()
                        .overflow_hidden()
                        .child("workspace")
                        .child("shared-notes.md")
                        .child("document"),
                ),
            )
            .child(
                v_flex().flex_1().min_h(px(0.0)).p_2().child(
                    Input::new(&self.editor)
                        .focus_bordered(false)
                        .h_full()
                        .w_full(),
                ),
            );

        let status = StatusBar::new()
            .left(connection_detail)
            .left(format!("Document: {DOCUMENT_ID}"))
            .right(format!("{line_count} lines"))
            .right("UTF-8")
            .right("Markdown")
            .right("GPUI Web");

        let body = h_flex().flex_1().min_h_0().child(sidebar).child(editor);

        v_flex()
            .size_full()
            .font(default_font)
            .child(body)
            .child(status)
            .children(dialog_layer)
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

fn set_connection_state(
    this: &WeakEntity<WorkspaceApp>,
    cx: &mut AsyncApp,
    state: ConnectionState,
) {
    this.update(cx, |this, cx| {
        this.connection = state;
        cx.notify();
    })
    .ok();
}

fn sync_payload(message: Vec<u8>) -> serde_json::Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(message);
    json!({"message": encoded})
}

async fn send_initial_sync(this: &WeakEntity<WorkspaceApp>, cx: &mut AsyncApp, channel: &Channel) {
    let message = this
        .update(cx, |this, _cx| this.initial_sync_message())
        .ok();
    if let Some(message) = message
        && channel.cast("yjs", sync_payload(message)).await.is_err()
    {
        set_connection_state(this, cx, ConnectionState::Reconnecting);
    }
}

async fn handle_channel_event(
    this: &WeakEntity<WorkspaceApp>,
    cx: &mut AsyncApp,
    channel: &Channel,
    event: ChannelEvent,
) {
    match event {
        ChannelEvent::Protocol(ProtocolEvent::Joined { .. }) => {
            set_connection_state(this, cx, ConnectionState::Online);
            send_initial_sync(this, cx, channel).await;
        }
        ChannelEvent::Protocol(ProtocolEvent::Message(frame)) if frame.event == "yjs" => {
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
            let responses = this
                .update_in(cx, |this, window, cx| {
                    this.apply_remote_message(&message, window, cx)
                })
                .unwrap_or_default();
            for response in responses {
                if channel.cast("yjs", sync_payload(response)).await.is_err() {
                    set_connection_state(this, cx, ConnectionState::Reconnecting);
                    break;
                }
            }
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
            set_connection_state(this, cx, ConnectionState::Reconnecting);
        }
        ChannelEvent::Protocol(ProtocolEvent::ChannelClosed { .. }) => {
            set_connection_state(this, cx, ConnectionState::Error("channel closed".into()));
        }
        ChannelEvent::JoinPayloadError(error) => {
            set_connection_state(this, cx, ConnectionState::Error(error.into()));
        }
        ChannelEvent::Lagged { .. } => {
            set_connection_state(this, cx, ConnectionState::Reconnecting);
            send_initial_sync(this, cx, channel).await;
            set_connection_state(this, cx, ConnectionState::Online);
        }
        ChannelEvent::Protocol(_) => {}
    }
}

fn payload_message(payload: &Payload) -> String {
    match payload {
        Payload::Json(value) => value.to_string(),
        Payload::Binary(bytes) => format!("binary response ({} bytes)", bytes.len()),
        Payload::Reply { status, response } => {
            format!("{status}: {}", payload_message(response))
        }
    }
}

fn handle_socket_event(this: &WeakEntity<WorkspaceApp>, cx: &mut AsyncApp, event: SocketEvent) {
    match event {
        SocketEvent::Connecting { attempt: 0 } => {
            set_connection_state(this, cx, ConnectionState::Connecting);
        }
        SocketEvent::Connecting { .. }
        | SocketEvent::Disconnected { .. }
        | SocketEvent::ReconnectScheduled { .. } => {
            set_connection_state(this, cx, ConnectionState::Reconnecting);
        }
        SocketEvent::ReconnectStopped { reason, .. } => {
            set_connection_state(
                this,
                cx,
                ConnectionState::Error(format!("reconnect stopped: {reason}").into()),
            );
        }
        SocketEvent::Closed => {
            set_connection_state(
                this,
                cx,
                ConnectionState::Error("channel client stopped".into()),
            );
        }
        SocketEvent::Connected | SocketEvent::Lagged { .. } => {}
    }
}

fn load_embedded_themes(cx: &mut App) {
    for theme in EMBEDDED_THEMES {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(theme)
            .expect("failed to load an embedded theme");
    }
}

fn select_theme(theme_name: &str, window: &mut Window, cx: &mut App) {
    let Some(next_theme) = ThemeRegistry::global(cx).themes().get(theme_name).cloned() else {
        return;
    };

    let mode = next_theme.mode;
    let theme = Theme::global_mut(cx);
    if mode.is_dark() {
        theme.dark_theme = next_theme;
    } else {
        theme.light_theme = next_theme;
    }
    Theme::change(mode, Some(window), cx);
    cx.refresh_windows();
}

fn main() {
    gpui_platform::web_init();
    log::set_max_level(log::LevelFilter::Info);

    if !web_sys::window().is_some_and(|window| window.is_secure_context()) {
        show_boot_error(
            "WebGPU requires a secure context. Open this application over HTTPS, or use http://localhost on the same device.",
        );
        return;
    }

    let application = gpui_platform::single_threaded_web()
        .with_assets(Assets::new(
            "https://longbridge.github.io/gpui-component/gallery/",
        ))
        .run_embedded(|cx: &mut App| {
            gpui_component::init(cx);
            load_embedded_themes(cx);
            cx.text_system()
                .add_fonts(vec![Cow::Borrowed(
                    include_bytes!("../fonts/NotoSansJP-Regular.otf").as_slice(),
                )])
                .expect("failed to load the bundled Japanese font");
            Theme::change(ThemeMode::Dark, None, cx);

            let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| WorkspaceApp::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );

            match window {
                Ok(_) => {
                    hide_boot_status();
                    cx.activate(true);
                }
                Err(error) => {
                    log::error!("failed to open GPUI Web window: {error:#}");
                    show_boot_error(&format!("Failed to open GPUI Web window: {error:#}"));
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
