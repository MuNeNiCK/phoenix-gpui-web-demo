use gpui::prelude::*;
use gpui::{IntoElement, RenderOnce, SharedString, px};
use gpui_component::{text::TextView, v_flex};

#[derive(IntoElement)]
pub(crate) struct PreviewPane {
    markdown: SharedString,
}

impl PreviewPane {
    pub(crate) fn new(markdown: SharedString) -> Self {
        Self { markdown }
    }
}

impl RenderOnce for PreviewPane {
    fn render(self, _window: &mut gpui::Window, _cx: &mut gpui::App) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                v_flex().w_full().flex_1().min_h(px(0.0)).child(
                    TextView::markdown("markdown-preview", self.markdown)
                        .scrollable(true)
                        .selectable(true)
                        .p_4(),
                ),
            )
    }
}
