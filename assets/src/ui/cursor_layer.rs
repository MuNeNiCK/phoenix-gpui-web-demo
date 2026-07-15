use gpui::prelude::*;
use gpui::{
    Bounds, Entity, IntoElement, RenderOnce, SharedString, TextAlign, TextRun, canvas, fill, point,
    px, size,
};
use gpui_component::{ActiveTheme, input::InputState};

use crate::collaboration::{RemoteCursor, awareness_color};

#[derive(IntoElement)]
pub(crate) struct RemoteCursorLayer {
    editor: Entity<InputState>,
    cursors: Vec<RemoteCursor>,
}

impl RemoteCursorLayer {
    pub(crate) fn new(editor: Entity<InputState>, cursors: Vec<RemoteCursor>) -> Self {
        Self { editor, cursors }
    }
}

impl RenderOnce for RemoteCursorLayer {
    fn render(self, _window: &mut gpui::Window, _cx: &mut gpui::App) -> impl IntoElement {
        let editor = self.editor;
        let cursors = self.cursors;
        canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                let cursors = {
                    let editor = editor.read(cx);
                    cursors
                        .into_iter()
                        .filter_map(|cursor| {
                            let bounds = editor.range_to_bounds(&(cursor.offset..cursor.offset))?;
                            Some((cursor.user, bounds))
                        })
                        .collect::<Vec<_>>()
                };

                for (user, bounds) in cursors {
                    let color = awareness_color(&user.color);
                    window.paint_quad(fill(
                        Bounds {
                            origin: bounds.origin,
                            size: size(px(2.0), bounds.size.height),
                        },
                        color,
                    ));
                    let label: SharedString = user.name.into();
                    let font_size = px(12.0);
                    let line_height = px(16.0);
                    let run = TextRun {
                        len: label.len(),
                        font: window.text_style().font(),
                        color: cx.theme().background,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line = window
                        .text_system()
                        .shape_line(label, font_size, &[run], None);
                    let label_origin = point(bounds.origin.x, bounds.origin.y - px(18.0));
                    window.paint_quad(fill(
                        Bounds {
                            origin: label_origin,
                            size: size(line.width() + px(8.0), px(18.0)),
                        },
                        color,
                    ));
                    let _ = line.paint(
                        label_origin + point(px(4.0), px(1.0)),
                        line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            },
        )
        .absolute()
        .size_full()
    }
}
