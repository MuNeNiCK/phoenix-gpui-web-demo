use gpui::prelude::*;
use gpui::{Context, Entity, FontFallbacks, SharedString, Subscription, Window};
use gpui_component::{
    ActiveTheme, IconName, IndexPath, Root, Selectable, Sizable, ThemeRegistry, WindowExt,
    avatar::{Avatar, AvatarGroup},
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants},
    dialog::DialogButtonProps,
    form::{field, v_form},
    h_flex,
    input::{Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
    select::{Select, SelectEvent, SelectState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    status_bar::StatusBar,
    tooltip::Tooltip,
    v_flex,
};

use crate::collaboration::{
    CollaborationEvent, CollaborationSession, ConnectionState, awareness_color,
};
use crate::documents::{
    DEFAULT_DOCUMENT_ID, WorkspaceDocument, document_id_from_title, document_title, load_documents,
    normalize_documents, save_documents,
};
use crate::theme::select_theme;
use crate::ui::{EditorPane, PreviewPane, RemoteCursorLayer};

const JAPANESE_FONT_FAMILY: &str = "Noto Sans JP";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Editor,
    Split,
    Preview,
}

pub(crate) struct WorkspaceApp {
    collaboration: Entity<CollaborationSession>,
    editor_focused: bool,
    editor: Entity<InputState>,
    search: Entity<InputState>,
    new_document_title: Entity<InputState>,
    theme_select: Entity<SelectState<Vec<SharedString>>>,
    documents: Vec<WorkspaceDocument>,
    document_id: String,
    view_mode: ViewMode,
    _subscriptions: Vec<Subscription>,
    _collaboration_subscription: Subscription,
}

impl WorkspaceApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        let new_document_title =
            cx.new(|cx| InputState::new(window, cx).placeholder("Document title"));
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
        let document_id = DEFAULT_DOCUMENT_ID.to_string();
        let documents = load_documents();
        let collaboration =
            cx.new(|cx| CollaborationSession::new(document_id.clone(), documents.clone(), cx));

        let editor_subscription = cx.subscribe_in(&editor, window, {
            let editor = editor.clone();
            move |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let value = editor.read(cx).value();
                    let selected_range = editor.read(cx).selected_range();
                    this.collaboration
                        .update(cx, |session, _| session.apply_local_text(value.as_ref()));
                    if this.editor_focused {
                        this.collaboration.update(cx, |session, _| {
                            session.publish_local_cursor(Some(selected_range))
                        });
                    }
                    cx.notify();
                }
                InputEvent::Focus => {
                    this.editor_focused = true;
                    let selected_range = editor.read(cx).selected_range();
                    this.collaboration.update(cx, |session, _| {
                        session.publish_local_cursor(Some(selected_range))
                    });
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.editor_focused = false;
                    this.collaboration
                        .update(cx, |session, _| session.publish_local_cursor(None));
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {}
            }
        });
        let editor_observer = cx.observe(&editor, |this, editor, cx| {
            if this.editor_focused {
                let selected_range = editor.read(cx).selected_range();
                this.collaboration.update(cx, |session, _| {
                    session.publish_local_cursor(Some(selected_range))
                });
            }
            cx.notify();
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

        let collaboration_subscription =
            cx.subscribe_in(&collaboration, window, |this, _, event, window, cx| {
                this.handle_collaboration_event(event, window, cx)
            });

        Self {
            collaboration,
            editor_focused: false,
            editor,
            search,
            new_document_title,
            theme_select,
            documents,
            document_id,
            view_mode: ViewMode::Split,
            _subscriptions: vec![
                editor_subscription,
                editor_observer,
                search_subscription,
                theme_subscription,
            ],
            _collaboration_subscription: collaboration_subscription,
        }
    }

    fn handle_collaboration_event(
        &mut self,
        event: &CollaborationEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CollaborationEvent::TextChanged(value) => {
                self.editor.update(cx, |editor, cx| {
                    if editor.value().as_ref() != value {
                        editor.set_value(value, window, cx);
                    }
                });
            }
            CollaborationEvent::DocumentsChanged(documents) => {
                self.apply_documents(documents.clone(), window, cx);
            }
            CollaborationEvent::PresenceChanged | CollaborationEvent::ConnectionChanged => {}
        }
        cx.notify();
    }

    fn open_document(&mut self, document_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_id == document_id {
            return;
        }
        let collaboration =
            cx.new(|cx| CollaborationSession::new(document_id.clone(), self.documents.clone(), cx));
        let subscription = cx.subscribe_in(&collaboration, window, |this, _, event, window, cx| {
            this.handle_collaboration_event(event, window, cx)
        });

        self.document_id = document_id;
        self.collaboration = collaboration;
        self._collaboration_subscription = subscription;
        self.editor_focused = false;
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        cx.notify();
    }

    fn create_document(
        &mut self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(title) = document_title(title) else {
            return false;
        };

        if let Some(document) = self
            .documents
            .iter()
            .find(|document| document.title.eq_ignore_ascii_case(&title))
            .cloned()
        {
            self.open_document(document.id, window, cx);
            return true;
        }

        let base_id = document_id_from_title(&title);
        let mut document_id = base_id.clone();
        let mut suffix = 2;
        while self
            .documents
            .iter()
            .any(|document| document.id == document_id)
        {
            document_id = format!("{base_id}-{suffix}");
            suffix += 1;
        }

        self.documents.push(WorkspaceDocument {
            id: document_id.clone(),
            title,
        });
        save_documents(&self.documents);
        self.open_document(document_id, window, cx);
        true
    }

    fn apply_documents(
        &mut self,
        documents: Vec<WorkspaceDocument>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if documents.is_empty() {
            return;
        }
        self.documents = normalize_documents(documents);
        save_documents(&self.documents);
        if !self
            .documents
            .iter()
            .any(|document| document.id == self.document_id)
        {
            self.open_document(DEFAULT_DOCUMENT_ID.to_string(), window, cx);
        } else {
            cx.notify();
        }
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

        let (connection, collaborators, remote_cursors) = {
            let collaboration = self.collaboration.read(cx);
            (
                collaboration.connection().clone(),
                collaboration.collaborators(),
                if self.view_mode == ViewMode::Preview {
                    Vec::new()
                } else {
                    collaboration.remote_cursors()
                },
            )
        };
        let connection_detail = match &connection {
            ConnectionState::Connecting => SharedString::from("Opening Phoenix Channel"),
            ConnectionState::Online => SharedString::from("All changes synchronized"),
            ConnectionState::Reconnecting => SharedString::from("Waiting for the server"),
            ConnectionState::Error(error) => format!("Synchronization error: {error}").into(),
        };
        let (connection_icon, connection_color) = match &connection {
            ConnectionState::Connecting => (IconName::LoaderCircle, theme.muted_foreground),
            ConnectionState::Online => (IconName::CircleCheck, theme.success),
            ConnectionState::Reconnecting => (IconName::LoaderCircle, theme.warning),
            ConnectionState::Error(_) => (IconName::TriangleAlert, theme.danger),
        };
        let theme_select = self.theme_select.clone();
        let view_mode = self.view_mode;
        let collaborator_count = collaborators.len();
        let collaborator_names = collaborators
            .iter()
            .map(|user| user.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let collaborator_avatars = collaborators
            .into_iter()
            .map(|user| {
                let color = awareness_color(&user.color);
                Avatar::new()
                    .name(user.name)
                    .small()
                    .bg(color.opacity(0.2))
                    .border_color(color)
            })
            .collect::<Vec<_>>();
        let search_query = self.search.read(cx).value();
        let query = search_query.trim().to_lowercase();
        let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
        let mut files = Vec::new();
        for document in self
            .documents
            .iter()
            .filter(|document| matches(&document.title))
        {
            let open_app = cx.entity();
            let document_id = document.id.clone();
            let delete_app = cx.entity();
            let delete_document_id = document.id.clone();
            let delete_document_title = document.title.clone();
            let deletable = document.id != DEFAULT_DOCUMENT_ID;
            files.push(
                SidebarMenuItem::new(document.title.clone())
                    .icon(IconName::File)
                    .active(document.id == self.document_id)
                    .on_click(move |_, window, cx| {
                        open_app.update(cx, |this, cx| {
                            this.open_document(document_id.clone(), window, cx)
                        });
                    })
                    .when(deletable, |item| {
                        item.suffix(move |_, _| {
                            let app = delete_app.clone();
                            let document_id = delete_document_id.clone();
                            let document_title = delete_document_title.clone();
                            Button::new(format!("delete-{document_id}"))
                                .icon(IconName::Delete)
                                .xsmall()
                                .ghost()
                                .danger()
                                .tooltip("Delete document")
                                .on_click(move |_, window, cx| {
                                    let app = app.clone();
                                    let document_id = document_id.clone();
                                    let description = format!(
                                        "Delete {document_title}? This action cannot be undone."
                                    );
                                    window.open_alert_dialog(cx, move |dialog, _, _| {
                                        dialog
                                            .title("Delete document")
                                            .description(description.clone())
                                            .button_props(
                                                DialogButtonProps::default()
                                                    .ok_variant(ButtonVariant::Danger)
                                                    .ok_text("Delete")
                                                    .cancel_text("Cancel")
                                                    .show_cancel(true),
                                            )
                                            .on_ok({
                                                let app = app.clone();
                                                let document_id = document_id.clone();
                                                move |_, _, cx| {
                                                    app.update(cx, |this, cx| {
                                                        this.collaboration.update(
                                                            cx,
                                                            |session, _| {
                                                                session.delete_document(
                                                                    document_id.clone(),
                                                                )
                                                            },
                                                        )
                                                    });
                                                    true
                                                }
                                            })
                                    });
                                })
                        })
                    }),
            );
        }
        if files.is_empty() {
            files.push(SidebarMenuItem::new("No matching files").disable(true));
        }

        let new_document_title = self.new_document_title.clone();
        let app = cx.entity();
        let new_document_button = Button::new("new-document")
            .icon(IconName::Plus)
            .tooltip("New document")
            .on_click(move |_, window, cx| {
                let input = new_document_title.clone();
                input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });

                let app = app.clone();
                window.open_dialog(cx, move |dialog, window, cx| {
                    input.update(cx, |input, cx| input.focus(window, cx));
                    dialog
                        .title("Create document")
                        .child(
                            v_form().child(
                                field()
                                    .label("Title")
                                    .child(Input::new(&input).prefix(IconName::File)),
                            ),
                        )
                        .on_ok({
                            let app = app.clone();
                            let input = input.clone();
                            move |_, window, cx| {
                                let title = input.read(cx).value();
                                app.update(cx, |this, cx| {
                                    this.create_document(title.as_ref(), window, cx)
                                })
                            }
                        })
                });
            });

        let sidebar = Sidebar::new("workspace-sidebar")
            .header(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Input::new(&self.search)
                            .prefix(IconName::Search)
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(new_document_button),
            )
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

        let editor = EditorPane::new(self.editor.clone());
        let preview = PreviewPane::new(value.clone());

        let mode_switcher = ButtonGroup::new("view-mode")
            .child(
                Button::new("editor-mode")
                    .icon(IconName::File)
                    .tooltip("Editor")
                    .selected(view_mode == ViewMode::Editor),
            )
            .child(
                Button::new("split-mode")
                    .icon(IconName::PanelRight)
                    .tooltip("Split")
                    .selected(view_mode == ViewMode::Split),
            )
            .child(
                Button::new("preview-mode")
                    .icon(IconName::Eye)
                    .tooltip("Preview")
                    .selected(view_mode == ViewMode::Preview),
            )
            .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                let Some(selected) = selected.first() else {
                    return;
                };
                this.view_mode = match selected {
                    0 => ViewMode::Editor,
                    1 => ViewMode::Split,
                    2 => ViewMode::Preview,
                    _ => return,
                };
                cx.notify();
            }));

        let toolbar = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(mode_switcher)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .when(collaborator_count > 1, |this| {
                        this.child(
                            h_flex()
                                .id("collaborators")
                                .child(
                                    AvatarGroup::new()
                                        .children(collaborator_avatars)
                                        .small()
                                        .limit(5)
                                        .ellipsis(),
                                )
                                .tooltip(move |window, cx| {
                                    Tooltip::new(collaborator_names.clone()).build(window, cx)
                                }),
                        )
                    })
                    .child(
                        h_flex()
                            .id("connection-status")
                            .size_8()
                            .items_center()
                            .justify_center()
                            .text_color(connection_color)
                            .child(connection_icon)
                            .tooltip({
                                let connection_detail = connection_detail.clone();
                                move |window, cx| {
                                    Tooltip::new(connection_detail.clone()).build(window, cx)
                                }
                            }),
                    ),
            );

        let document_content = match view_mode {
            ViewMode::Editor => editor.into_any_element(),
            ViewMode::Preview => preview.into_any_element(),
            ViewMode::Split => h_resizable("document-panes")
                .child(resizable_panel().child(editor))
                .child(resizable_panel().child(preview))
                .into_any_element(),
        };

        let document = v_flex()
            .size_full()
            .child(toolbar)
            .child(v_flex().flex_1().min_h_0().child(document_content));

        let status = StatusBar::new()
            .left(connection_detail)
            .left(format!("Document: {}", self.document_id))
            .right(format!("{line_count} lines"))
            .right("UTF-8")
            .right("Markdown")
            .right("GPUI Web");

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(sidebar)
            .child(v_flex().flex_1().min_w_0().h_full().child(document));

        let remote_cursor_layer = RemoteCursorLayer::new(self.editor.clone(), remote_cursors);

        v_flex()
            .size_full()
            .relative()
            .font(default_font)
            .child(body)
            .child(status)
            .child(remote_cursor_layer)
            .children(dialog_layer)
    }
}
