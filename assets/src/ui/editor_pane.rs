use gpui::prelude::*;
use gpui::{Entity, IntoElement, RenderOnce, px};
use gpui_component::{
    input::{Input, InputState},
    v_flex,
};

#[derive(IntoElement)]
pub(crate) struct EditorPane {
    editor: Entity<InputState>,
}

impl EditorPane {
    pub(crate) fn new(editor: Entity<InputState>) -> Self {
        Self { editor }
    }
}

impl RenderOnce for EditorPane {
    fn render(self, _window: &mut gpui::Window, _cx: &mut gpui::App) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                v_flex().flex_1().min_h(px(0.0)).child(
                    Input::new(&self.editor)
                        .bordered(false)
                        .focus_bordered(false)
                        .h_full()
                        .w_full(),
                ),
            )
    }
}
