use base64::Engine;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{FutureExt, StreamExt, select};
use gpui::prelude::*;
use gpui::{
    App, AsyncApp, Bounds, Context, Entity, FontWeight, SharedString, Subscription, Task,
    WeakEntity, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::{
    ActiveTheme, Root, Selectable, Sizable, Theme, ThemeMode,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    tag::Tag,
};
use phoenix_channel_client::{
    Channel, ChannelEvent, Options as ChannelOptions, Socket, SocketEvent, static_join_payload,
};
use phoenix_channel_runtime::{Payload, ProtocolEvent};
use phoenix_channel_runtime_web::{WebConnector, WebTimer};
use serde_json::json;
use std::cell::RefCell;
use std::time::Duration;
use yrs::sync::{Awareness, DefaultProtocol, Message, Protocol, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, OffsetKind, Options, ReadTxn, Text as YText, TextRef, Transact};

const DOCUMENT_ID: &str = "shared-notes";
const TOPIC: &str = "documents:shared-notes";

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarSection {
    Files,
    Search,
    Sync,
}

struct WorkspaceApp {
    awareness: Awareness,
    text: TextRef,
    editor: Entity<InputState>,
    search: Entity<InputState>,
    connection: ConnectionState,
    sidebar_section: SidebarSection,
    sidebar_collapsed: bool,
    workspace_open: bool,
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

        let socket_task = Self::socket_task(cx, receiver);

        Self {
            awareness,
            text,
            editor,
            search,
            connection: ConnectionState::Connecting,
            sidebar_section: SidebarSection::Files,
            sidebar_collapsed: false,
            workspace_open: true,
            outbound,
            _subscriptions: vec![editor_subscription, search_subscription],
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let border = theme.border;
        let sidebar = theme.sidebar;
        let sidebar_foreground = theme.sidebar_foreground;
        let tab_bar = theme.tab_bar;
        let tab_active = theme.tab_active;
        let status_bar = theme.status_bar;
        let primary = theme.primary;
        let font = theme.font_family.clone();
        let mono_font = theme.mono_font_family.clone();
        let is_dark = theme.is_dark();
        let value = self.editor.read(cx).value();
        let line_count = value.lines().count().max(1);
        let character_count = value.chars().count();

        let (connection_tag, connection_detail) = match &self.connection {
            ConnectionState::Connecting => (
                Tag::warning()
                    .small()
                    .outline()
                    .rounded_full()
                    .child("Connecting"),
                SharedString::from("Opening Phoenix Channel"),
            ),
            ConnectionState::Online => (
                Tag::success()
                    .small()
                    .outline()
                    .rounded_full()
                    .child("Connected"),
                SharedString::from("All changes synchronized"),
            ),
            ConnectionState::Reconnecting => (
                Tag::warning()
                    .small()
                    .outline()
                    .rounded_full()
                    .child("Reconnecting"),
                SharedString::from("Waiting for the server"),
            ),
            ConnectionState::Error(error) => (
                Tag::danger()
                    .small()
                    .outline()
                    .rounded_full()
                    .child("Offline"),
                format!("Synchronization error: {error}").into(),
            ),
        };

        let title_bar = div()
            .h(px(44.0))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(5.0))
                            .bg(primary)
                            .text_color(theme.primary_foreground)
                            .font_weight(FontWeight::BOLD)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child("P"),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Phoenix Workspace"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(muted_foreground)
                            .child("workspace / shared-notes.md"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(connection_tag)
                    .child(
                        Button::new("theme-toggle")
                            .small()
                            .ghost()
                            .label(if is_dark { "Light" } else { "Dark" })
                            .on_click(|_, window, cx| {
                                let next = if cx.theme().is_dark() {
                                    ThemeMode::Light
                                } else {
                                    ThemeMode::Dark
                                };
                                Theme::change(next, Some(window), cx);
                            }),
                    ),
            );

        let activity_button = |id: &'static str, label: &'static str, section: SidebarSection| {
            Button::new(id)
                .small()
                .ghost()
                .compact()
                .w(px(36.0))
                .h(px(34.0))
                .font_family(mono_font.clone())
                .label(label)
                .selected(self.sidebar_section == section && !self.sidebar_collapsed)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sidebar_section = section;
                    this.sidebar_collapsed = false;
                    cx.notify();
                }))
        };

        let activity_bar = div()
            .w(px(52.0))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(5.0))
            .py(px(7.0))
            .bg(sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                Button::new("sidebar-toggle")
                    .small()
                    .ghost()
                    .compact()
                    .w(px(36.0))
                    .h(px(32.0))
                    .font_family(mono_font.clone())
                    .label(if self.sidebar_collapsed { ">" } else { "<" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        cx.notify();
                    })),
            )
            .child(div().w(px(28.0)).h(px(1.0)).my(px(2.0)).bg(border))
            .child(activity_button(
                "activity-files",
                "F",
                SidebarSection::Files,
            ))
            .child(activity_button(
                "activity-search",
                "/",
                SidebarSection::Search,
            ))
            .child(activity_button("activity-sync", "S", SidebarSection::Sync));

        let file_row = |id: &'static str, name: &'static str, active: bool| {
            div()
                .id(id)
                .h(px(27.0))
                .w_full()
                .flex()
                .items_center()
                .rounded(px(4.0))
                .px(px(8.0))
                .text_size(px(12.0))
                .text_color(if active {
                    theme.sidebar_accent_foreground
                } else {
                    muted_foreground
                })
                .when(active, |this| this.bg(theme.sidebar_accent))
                .hover(|this| this.bg(theme.sidebar_accent))
                .child(name)
        };

        let files_panel = div()
            .flex_1()
            .min_h(px(0.0))
            .p(px(6.0))
            .child(
                div()
                    .id("workspace-toggle")
                    .h(px(28.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .px(px(7.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|this| this.bg(theme.sidebar_accent))
                    .child(if self.workspace_open {
                        "v  WORKSPACE"
                    } else {
                        ">  WORKSPACE"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace_open = !this.workspace_open;
                        cx.notify();
                    })),
            )
            .when(self.workspace_open, |this| {
                this.child(
                    div()
                        .ml(px(12.0))
                        .mt(px(2.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(file_row("file-shared-notes", "M  shared-notes.md", true))
                        .child(file_row("file-readme", "-  README.md", false))
                        .child(file_row("file-lib", ">  lib", false))
                        .child(file_row("file-config", ">  config", false)),
                )
            });

        let search_query = self.search.read(cx).value();
        let search_matches = search_query.trim().is_empty()
            || "shared-notes.md".contains(&search_query.to_lowercase());
        let search_panel = div()
            .flex_1()
            .min_h(px(0.0))
            .p(px(10.0))
            .child(Input::new(&self.search).small().w_full())
            .child(
                div()
                    .mt(px(12.0))
                    .mb(px(6.0))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(muted_foreground)
                    .child("RESULTS"),
            )
            .when(search_matches, |this| {
                this.child(
                    div()
                        .rounded(px(5.0))
                        .bg(theme.sidebar_accent)
                        .px(px(9.0))
                        .py(px(7.0))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(sidebar_foreground)
                                .child("shared-notes.md"),
                        )
                        .child(
                            div()
                                .mt(px(2.0))
                                .text_size(px(10.0))
                                .text_color(muted_foreground)
                                .child("workspace"),
                        ),
                )
            })
            .when(!search_matches, |this| {
                this.child(
                    div()
                        .py(px(18.0))
                        .text_center()
                        .text_size(px(12.0))
                        .text_color(muted_foreground)
                        .child("No matching files"),
                )
            });

        let sync_color = match &self.connection {
            ConnectionState::Online => theme.success,
            ConnectionState::Error(_) => theme.danger,
            ConnectionState::Connecting | ConnectionState::Reconnecting => theme.warning,
        };
        let sync_panel = div()
            .flex_1()
            .min_h(px(0.0))
            .p(px(12.0))
            .child(
                div()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(theme.sidebar_border)
                    .bg(background)
                    .p(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().size(px(8.0)).rounded_full().bg(sync_color))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(connection_detail.clone()),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(10.0))
                            .text_size(px(11.0))
                            .text_color(muted_foreground)
                            .child("Phoenix Channel"),
                    )
                    .child(
                        div()
                            .mt(px(3.0))
                            .font_family(mono_font.clone())
                            .text_size(px(11.0))
                            .child(TOPIC),
                    ),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(px(11.0))
                    .text_color(muted_foreground)
                    .line_height(px(17.0))
                    .child("Edits are stored locally first and synchronized when the channel is available."),
            );

        let sidebar_title = match self.sidebar_section {
            SidebarSection::Files => "EXPLORER",
            SidebarSection::Search => "SEARCH",
            SidebarSection::Sync => "SYNC",
        };
        let sidebar_content = match self.sidebar_section {
            SidebarSection::Files => files_panel.into_any_element(),
            SidebarSection::Search => search_panel.into_any_element(),
            SidebarSection::Sync => sync_panel.into_any_element(),
        };

        let explorer = div()
            .w(px(244.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .px(px(12.0))
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .text_size(px(10.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(muted_foreground)
                    .child(sidebar_title),
            )
            .child(sidebar_content)
            .child(
                div()
                    .p(px(11.0))
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(muted_foreground)
                            .child("CURRENT DOCUMENT"),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(11.0))
                            .text_color(sidebar_foreground)
                            .child(format!("{line_count} lines · {character_count} characters")),
                    ),
            );

        let editor = div()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .bg(background)
            .child(
                div()
                    .h(px(36.0))
                    .w_full()
                    .flex()
                    .items_end()
                    .bg(tab_bar)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .h_full()
                            .min_w(px(180.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(12.0))
                            .bg(tab_active)
                            .border_r_1()
                            .border_color(border)
                            .text_size(px(13.0))
                            .child("M")
                            .child("shared-notes.md")
                            .child(div().ml_auto().text_color(muted_foreground).child("×")),
                    ),
            )
            .child(
                div()
                    .h(px(30.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .px(px(14.0))
                    .border_b_1()
                    .border_color(border)
                    .text_size(px(12.0))
                    .text_color(muted_foreground)
                    .child("workspace  /  shared-notes.md  /  document"),
            )
            .child(
                div().flex_1().min_h(px(0.0)).p(px(8.0)).child(
                    Input::new(&self.editor)
                        .h_full()
                        .w_full()
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false),
                ),
            );

        let status = div()
            .h(px(25.0))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(10.0))
            .bg(status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_size(px(11.0))
            .text_color(muted_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(connection_detail)
                    .child(format!("Document: {DOCUMENT_ID}")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(format!("{line_count} lines"))
                    .child("UTF-8")
                    .child("Markdown")
                    .child("GPUI Web"),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .font_family(font)
            .child(title_bar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .flex()
                    .child(activity_bar)
                    .when(!self.sidebar_collapsed, |this| this.child(explorer))
                    .child(editor),
            )
            .child(status)
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

fn main() {
    gpui_platform::web_init();
    log::set_max_level(log::LevelFilter::Info);

    if !web_sys::window().is_some_and(|window| window.is_secure_context()) {
        show_boot_error(
            "WebGPU requires a secure context. Open this application over HTTPS, or use http://localhost on the same device.",
        );
        return;
    }

    let application = gpui_platform::single_threaded_web().run_embedded(|cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.global_mut::<Theme>().font_family = "IBM Plex Sans".into();
        cx.global_mut::<Theme>().mono_font_family = "Lilex".into();

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
