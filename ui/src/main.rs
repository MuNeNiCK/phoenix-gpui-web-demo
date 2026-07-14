use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, FontWeight, SharedString, Task, Window, WindowBounds, WindowOptions, div,
    px, size,
};
use guise::prelude::*;
use serde::Deserialize;
use std::cell::RefCell;

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

#[derive(Clone, Deserialize)]
struct BackendInfo {
    service: String,
    status: String,
    elixir: String,
    otp: String,
}

enum BackendState {
    Idle,
    Loading,
    Online(BackendInfo),
    Error(SharedString),
}

struct DemoApp {
    backend: BackendState,
    clicks: u32,
    request: Option<Task<()>>,
}

impl DemoApp {
    fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            backend: BackendState::Idle,
            clicks: 0,
            request: None,
        }
    }

    fn request_status(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let state = match gloo_net::http::Request::get("/api/status").send().await {
                Ok(response) => match response.json::<BackendInfo>().await {
                    Ok(info) => BackendState::Online(info),
                    Err(error) => BackendState::Error(error.to_string().into()),
                },
                Err(error) => BackendState::Error(error.to_string().into()),
            };

            this.update(cx, |this, cx| {
                this.backend = state;
                cx.notify();
            })
            .ok();
        })
    }

    fn refresh_backend(&mut self, cx: &mut Context<Self>) {
        self.backend = BackendState::Loading;
        self.request = Some(Self::request_status(cx));
        cx.notify();
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

        let (status_badge, status_text, runtime_text) = match &self.backend {
            BackendState::Idle => (
                Badge::new("ready").color(ColorName::Blue),
                SharedString::from("Ready to connect to the Phoenix API."),
                SharedString::from("Use the button below to fetch live runtime data"),
            ),
            BackendState::Loading => (
                Badge::new("connecting").color(ColorName::Yellow),
                SharedString::from("Waiting for the Phoenix API response..."),
                SharedString::from("Elixir runtime: --"),
            ),
            BackendState::Online(info) => (
                Badge::new(info.status.clone()).color(ColorName::Teal),
                format!("{} same-origin connection active", info.service).into(),
                format!("Elixir {} / OTP {} / browser WASM", info.elixir, info.otp).into(),
            ),
            BackendState::Error(error) => (
                Badge::new("offline").color(ColorName::Red),
                format!("API request failed: {error}").into(),
                SharedString::from("Start Phoenix, then try the connection again"),
            ),
        };

        let click_label: SharedString = format!("Local click count: {}", self.clicks).into();

        let header = Group::new()
            .justify(Justify::Between)
            .child(
                Stack::new()
                    .gap(Size::Xs)
                    .child(Title::new("Elixir + GPUI Web").order(1))
                    .child(Text::new("Browser-native Rust/WASM demo").dimmed()),
            )
            .child(
                Group::new()
                    .gap(Size::Sm)
                    .child(Badge::new("WASM").color(ColorName::Grape))
                    .child(
                        Button::new("theme-toggle", if is_dark { "Light" } else { "Dark" })
                            .variant(Variant::Default)
                            .on_click(cx.listener(|_, _, window, cx| {
                                let next = cx.global::<Theme>().scheme.toggled();
                                cx.global_mut::<Theme>().scheme = next;
                                window.refresh();
                            })),
                    ),
            );

        let backend_card = Paper::new()
            .with_border(true)
            .shadow(Size::Sm)
            .padding(Size::Lg)
            .child(
                Stack::new()
                    .gap(Size::Sm)
                    .child(
                        Group::new()
                            .justify(Justify::Between)
                            .child(Title::new("Backend status").order(3))
                            .child(status_badge),
                    )
                    .child(Text::new(status_text))
                    .child(Text::new(runtime_text).size(Size::Sm).dimmed())
                    .child(
                        Button::new("refresh-backend", "Connect to Phoenix API")
                            .color(ColorName::Teal)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_backend(cx);
                            })),
                    ),
            );

        let interaction_card = Paper::new()
            .with_border(true)
            .shadow(Size::Sm)
            .padding(Size::Lg)
            .child(
                Stack::new()
                    .gap(Size::Sm)
                    .child(Title::new("Browser interaction").order(3))
                    .child(Text::new(
                        "GPUI owns state updates and rendering inside the browser.",
                    ))
                    .child(Text::new(click_label).bold())
                    .child(
                        Button::new("count-up", "+1")
                            .color(ColorName::Grape)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clicks += 1;
                                cx.notify();
                            })),
                    ),
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
                    .max_w(px(1040.0))
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
                                                    .child("Phoenix JSON API -> fetch -> Rust/WASM -> GPUI Web canvas"),
                                            ),
                                    ),
                            )
                            .child(
                                Group::new()
                                    .align(Align::Stretch)
                                    .grow(true)
                                    .child(backend_card)
                                    .child(interaction_card),
                            )
                            .child(
                                Text::new(
                                    "No Rustler: Rust runs in the browser as WebAssembly.",
                                )
                                .size(Size::Sm)
                                .dimmed(),
                            ),
                    ),
            )
    }
}

fn main() {
    gpui_platform::web_init();
    // The single-threaded dispatcher is intentional for this demo. Keep the
    // console focused on actionable initialization errors.
    log::set_max_level(log::LevelFilter::Error);

    // GPUI Web's multithreaded dispatcher is still experimental and currently
    // races wasm-bindgen worker callbacks in Chromium. Keep this demo on the
    // browser's foreground executor; the API request remains asynchronous.
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
