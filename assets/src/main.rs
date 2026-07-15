mod app;
mod collaboration;
mod documents;
mod text_offsets;
mod theme;
mod ui;

use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, ThemeMode};
use gpui_component_assets::Assets;
use std::borrow::Cow;
use std::cell::RefCell;

use app::WorkspaceApp;
use theme::load_embedded_themes;

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
