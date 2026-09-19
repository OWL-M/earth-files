// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! A multi-line text editor
//!
//! Vendored from pop-os/libcosmic d9431dc, src/widget/text_editor.rs
//!
//! `wayland_platform`, which selects between the Wayland popup and the
//! in-window overlay, is mapped to this crate's `wayland` feature, and the
//! windowing system is read from `crate::ui::shell::runner`.
//!
//! ## The `text_editor` delta
//!
//! Upstream `iced_widget 0.14`'s `text_editor` already has native selection and
//! `Binding::Copy`. libcosmic's iced fork (fork
//! `iced/widget/src/text_editor.rs`, 1702 lines vs upstream's 1526) adds
//! right-click context-menu integration: three private `State` fields
//! (`context_menu_position`, `clipboard_has_text`, `pending_edit`) and a block
//! at the head of `update` that runs *before* the widget's `on_edit` gate.
//!
//! This wrapper implements the additions without duplicating the upstream
//! editor's 1526 lines. It owns the tree state (`EditorWrapperState`) and runs
//! before the inner editor's `update`. The three fields live in that state,
//! and [`TextEditor::update`] runs the fork's pre-gate block before forwarding
//! the event, preserving the fork's observable behaviour.
//!
//! Upstream's `TextEditor::update` returns immediately when `on_edit` is
//! `None`. The fork has the same gate: it dispatches `pending_edit` only
//! through `on_edit`. Neither of this app's two call sites (`tab.rs:2247`,
//! `tab.rs:4902`, the text-file preview)
//! sets `on_action`, so on both, under libcosmic exactly as here, the editor
//! never focuses through the gated path and Select All is inert. The fork's
//! pre-gate right-click block still runs here, so the menu opens with Copy
//! disabled because there is no selection.

pub use iced::widget::text_editor::{
    Action, Binding, Catalog, Content, Cursor, Edit, KeyPress, Line, LineEnding, Motion,
    Position, Selection, State, Status, Style, StyleFn,
};
// The fork re-exported `Id` from `text_editor`; upstream's widget ids are one
// type, `iced::advanced::widget::Id`.
pub use iced::advanced::widget::Id;

use crate::ui::widget::menu::MenuBarState;

use iced_core::event::Event;
use iced_core::text::highlighter;
use iced_core::widget::Widget;
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Clipboard, Layout, Length, Point, Rectangle, Shell, Size, Vector, mouse, overlay, renderer,
    window,
};
use std::rc::Rc;

type InnerEditor<'a, Message> =
    iced::widget::TextEditor<'a, highlighter::PlainText, Message, crate::ui::Theme, iced::Renderer>;

pub struct TextEditor<'a, Message> {
    inner: InnerEditor<'a, Message>,
    has_context_menu: bool,
    window_id: window::Id,
    // `selected_text`/`has_text` read the content directly, as the fork's impl
    // does (`self.content.selection()`, `!self.content.is_empty()`).
    content: &'a Content<iced::Renderer>,
    // Kept alongside the copy handed to the inner editor so `update` can
    // dispatch a pending edit through it, exactly as the fork does. `Rc`
    // because `on_action` takes a non-`Clone` `impl Fn`.
    on_action: Option<Rc<dyn Fn(Action) -> Message + 'a>>,
}

/// The wrapper's tree state.
///
/// `context_menu_position`, `clipboard_has_text` and `pending_edit` are the
/// fork's three private `text_editor::State` additions, held here instead.
/// See the module docs.
struct EditorWrapperState {
    menu_bar_state: MenuBarState,
    pending_action: crate::ui::widget::text_context_menu::PendingAction,
    context_menu_position: Option<iced_core::Point>,
    clipboard_has_text: bool,
    focused: bool,
    pending_edit: Option<Action>,
}

impl EditorWrapperState {
    /// The fork's `State::clear_focus`, for the parts this wrapper owns.
    fn clear_focus(&mut self) {
        self.focused = false;
        self.context_menu_position = None;
    }
}

impl<'a, Message: Clone + 'static> TextEditor<'a, Message> {
    /// Creates a new [`TextEditor`] from the given [`Content`].
    pub fn new(content: &'a Content<iced::Renderer>) -> Self {
        Self {
            inner: iced::widget::text_editor(content),
            has_context_menu: true,
            window_id: crate::ui::widget::text_context_menu::current_window_id(),
            content,
            on_action: None,
        }
    }

    /// Controls whether the right-click context menu is shown.
    ///
    /// The context menu is enabled by default. Pass `false` to disable it.
    pub fn context_menu(mut self, enabled: bool) -> Self {
        self.has_context_menu = enabled;
        self
    }

    pub fn id(mut self, id: impl Into<iced_core::widget::Id>) -> Self {
        self.inner = self.inner.id(id);
        self
    }

    pub fn placeholder(mut self, placeholder: impl iced_core::text::IntoFragment<'a>) -> Self {
        self.inner = self.inner.placeholder(placeholder);
        self
    }

    pub fn width(mut self, width: impl Into<iced_core::Pixels>) -> Self {
        self.inner = self.inner.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.inner = self.inner.height(height);
        self
    }

    pub fn min_height(mut self, min_height: impl Into<iced_core::Pixels>) -> Self {
        self.inner = self.inner.min_height(min_height);
        self
    }

    pub fn max_height(mut self, max_height: impl Into<iced_core::Pixels>) -> Self {
        self.inner = self.inner.max_height(max_height);
        self
    }

    pub fn on_action(mut self, on_edit: impl Fn(Action) -> Message + 'a) -> Self {
        // The wrapper needs its own handle to publish a context-menu edit, and
        // the inner editor needs one to drive normal editing, so the closure is
        // shared rather than moved. This is what makes `is_editable()` true.
        let on_edit: Rc<dyn Fn(Action) -> Message + 'a> = Rc::new(on_edit);
        let for_inner = Rc::clone(&on_edit);
        self.inner = self.inner.on_action(move |action| (for_inner)(action));
        self.on_action = Some(on_edit);
        self
    }

    pub fn font(
        mut self,
        font: impl Into<<iced::Renderer as iced_core::text::Renderer>::Font>,
    ) -> Self {
        self.inner = self.inner.font(font);
        self
    }

    pub fn size(mut self, size: impl Into<iced_core::Pixels>) -> Self {
        self.inner = self.inner.size(size);
        self
    }

    pub fn line_height(mut self, line_height: impl Into<iced_core::text::LineHeight>) -> Self {
        self.inner = self.inner.line_height(line_height);
        self
    }

    pub fn padding(mut self, padding: impl Into<iced_core::Padding>) -> Self {
        self.inner = self.inner.padding(padding);
        self
    }

    pub fn wrapping(mut self, wrapping: iced_core::text::Wrapping) -> Self {
        self.inner = self.inner.wrapping(wrapping);
        self
    }

    pub fn key_binding(
        mut self,
        key_binding: impl Fn(KeyPress) -> Option<Binding<Message>> + 'a,
    ) -> Self {
        self.inner = self.inner.key_binding(key_binding);
        self
    }

    #[must_use]
    pub fn style(mut self, style: impl Fn(&crate::ui::Theme, Status) -> Style + 'a) -> Self
    where
        <crate::ui::Theme as Catalog>::Class<'a>:
            From<iced::widget::text_editor::StyleFn<'a, crate::ui::Theme>>,
    {
        self.inner = self.inner.style(style);
        self
    }

    fn uses_popup_context_menu(&self) -> bool {
        if matches!(
            crate::ui::shell::runner::windowing_system(),
            Some(crate::ui::shell::runner::WindowingSystem::Wayland)
        ) {
            return true;
        }
        false
    }
}

/// Creates a new [`TextEditor`] from the given [`Content`].
pub fn text_editor<'a, Message: Clone + 'static>(
    content: &'a Content<iced::Renderer>,
) -> TextEditor<'a, Message> {
    TextEditor::new(content)
}

fn ew<'x, Message>(
    inner: &'x InnerEditor<'_, Message>,
) -> &'x dyn Widget<Message, crate::ui::Theme, iced::Renderer> {
    inner
}

fn ew_mut<'x, Message>(
    inner: &'x mut InnerEditor<'_, Message>,
) -> &'x mut dyn Widget<Message, crate::ui::Theme, iced::Renderer> {
    inner
}

impl<'a, Message: Clone + 'static> Widget<Message, crate::ui::Theme, iced::Renderer>
    for TextEditor<'a, Message>
{
    fn tag(&self) -> tree::Tag {
        if self.has_context_menu {
            tree::Tag::of::<EditorWrapperState>()
        } else {
            ew::<Message>(&self.inner).tag()
        }
    }

    fn state(&self) -> tree::State {
        if self.has_context_menu {
            tree::State::new(EditorWrapperState {
                menu_bar_state: MenuBarState::default(),
                pending_action: crate::ui::widget::text_context_menu::pending_action(),
                context_menu_position: None,
                clipboard_has_text: false,
                focused: false,
                pending_edit: None,
            })
        } else {
            ew::<Message>(&self.inner).state()
        }
    }

    fn children(&self) -> Vec<Tree> {
        if self.has_context_menu {
            vec![Tree::new(ew::<Message>(&self.inner))]
        } else {
            ew::<Message>(&self.inner).children()
        }
    }

    fn diff(&self, tree: &mut Tree) {
        if self.has_context_menu {
            if let Some(child) = tree.children.first_mut() {
                child.diff(
                    &self.inner as &dyn Widget<Message, crate::ui::Theme, iced::Renderer>,
                );
            }
        } else {
            ew::<Message>(&self.inner).diff(tree);
        }
    }

    fn size(&self) -> Size<Length> {
        ew::<Message>(&self.inner).size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        let inner_tree = if self.has_context_menu {
            &mut tree.children[0]
        } else {
            tree
        };
        ew_mut::<Message>(&mut self.inner).layout(inner_tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &crate::ui::Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let inner_tree = if self.has_context_menu {
            &tree.children[0]
        } else {
            tree
        };
        ew::<Message>(&self.inner).draw(
            inner_tree, renderer, theme, defaults, layout, cursor, viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.has_context_menu {
            // ---------------------------------------------------------------
            // The fork's pre-gate block (`iced/widget/src/text_editor.rs:716`).
            // Run this before the inner editor sees the event. Upstream and
            // the fork both return immediately from `update` when `on_edit`
            // is `None`, so only this block runs for a read-only editor.
            // ---------------------------------------------------------------
            {
                let text_bounds = layout.bounds();
                let state = tree.state.downcast_mut::<EditorWrapperState>();

                if matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(_))
                        | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
                ) && cursor.position_over(text_bounds).is_none()
                {
                    state.clear_focus();
                }

                match event {
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                        if let Some(pos) = cursor.position_over(text_bounds) {
                            state.focused = true;
                            state.context_menu_position = Some(pos);
                            state.clipboard_has_text =
                                crate::ui::widget::text::clipboard_has_text(clipboard);
                            shell.capture_event();
                            return;
                        }
                    }
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                        if state.context_menu_position.take().is_some() {
                            shell.capture_event();
                            return;
                        }
                    }
                    _ => {}
                }
            }

            ew_mut::<Message>(&mut self.inner).update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );

            // The fork dispatches a context-menu edit through `on_edit`, so the
            // app performs it on its `Content` (fork `:760`). With no
            // `on_action`, the edit is dropped, as it is in the fork.
            {
                let pending = tree
                    .state
                    .downcast_mut::<EditorWrapperState>()
                    .pending_edit
                    .take();
                if let (Some(action), Some(on_edit)) = (pending, self.on_action.as_ref()) {
                    shell.publish((on_edit)(action));
                }
            }

            use crate::ui::widget::text::HasSelectableText;
            if self.is_focused(tree)
                && matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(_))
                        | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
                )
                && cursor.is_over(layout.bounds())
            {
                crate::ui::widget::text_input::notify_focus_change();
            }

            if self.uses_popup_context_menu() {
                if self.context_menu_position(tree).is_some() {
                    let selected_text = self.selected_text(tree);
                    let has_selection = selected_text.is_some();
                    let click_position = self.context_menu_position(tree).unwrap();
                    let wrapper_state = tree.state.downcast_ref::<EditorWrapperState>();
                    let menu_bar_state = wrapper_state.menu_bar_state.clone();
                    let pending_action = wrapper_state.pending_action.clone();

                    crate::ui::widget::text_context_menu::create_text_context_popup(
                        click_position,
                        selected_text,
                        self.is_editable(),
                        has_selection,
                        self.has_text(tree),
                        self.clipboard_has_text(tree),
                        &menu_bar_state,
                        &pending_action,
                        renderer,
                        viewport,
                        cursor,
                        self.window_id,
                    );

                    self.set_context_menu_position(tree, None);
                }

                // Process deferred actions from the popup.
                {
                    let wrapper_state = tree.state.downcast_ref::<EditorWrapperState>();
                    let pending_action = wrapper_state.pending_action.clone();
                    if let Some(action) =
                        crate::ui::widget::text_context_menu::take_pending_action(&pending_action)
                    {
                        match action {
                            crate::ui::widget::text_context_menu::TextCtxAction::Copy => {}
                            crate::ui::widget::text_context_menu::TextCtxAction::Cut => {
                                self.delete_selection(tree);
                            }
                            crate::ui::widget::text_context_menu::TextCtxAction::Paste => {
                                let content: String = clipboard
                                    .read(iced_core::clipboard::Kind::Standard)
                                    .unwrap_or_default();
                                self.paste_text(tree, &content);
                            }
                            crate::ui::widget::text_context_menu::TextCtxAction::SelectAll => {
                                self.select_all(tree);
                            }
                        }
                    }
                }

                // Dismiss popup on outside click / Escape.
                let wrapper_state = tree.state.downcast_ref::<EditorWrapperState>();
                let menu_bar_state = wrapper_state.menu_bar_state.clone();
                crate::ui::widget::text_context_menu::dismiss_popup_on_event(
                    &menu_bar_state,
                    event,
                    self.window_id,
                );
            }
        } else {
            ew_mut::<Message>(&mut self.inner).update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let inner_tree = if self.has_context_menu {
            &tree.children[0]
        } else {
            tree
        };
        ew::<Message>(&self.inner).mouse_interaction(inner_tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, iced::Renderer>> {
        if self.has_context_menu {
            if !self.uses_popup_context_menu() {
                use crate::ui::widget::text::HasSelectableText;
                if self.context_menu_position(tree).is_some() {
                    let menu_bar_state = tree
                        .state
                        .downcast_ref::<EditorWrapperState>()
                        .menu_bar_state
                        .clone();
                    // The menu now reads the wrapper's own state, so it is the
                    // wrapper's tree that goes in, not the inner editor's.
                    return crate::ui::widget::text_context_menu::context_menu_overlay(
                        &*self,
                        tree,
                        None,
                        translation,
                        menu_bar_state,
                    );
                }
            }

            ew_mut::<Message>(&mut self.inner).overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            )
        } else {
            ew_mut::<Message>(&mut self.inner).overlay(
                tree,
                layout,
                renderer,
                viewport,
                translation,
            )
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        let inner_tree = if self.has_context_menu {
            &mut tree.children[0]
        } else {
            tree
        };
        ew_mut::<Message>(&mut self.inner).operate(inner_tree, layout, renderer, operation);
    }

}

// ---------------------------------------------------------------------------
// Ported from the fork's impl (`iced/widget/src/text_editor.rs:1636`), which
// could not be written against upstream's `TextEditor` because it reads three
// private, fork-only `State` fields. Those live in `EditorWrapperState` here,
// so the impl is on the wrapper and reads the wrapper's own tree state.
// ---------------------------------------------------------------------------

impl<Message> crate::ui::widget::text::HasSelectableText for TextEditor<'_, Message> {
    fn selected_text(&self, _tree: &Tree) -> Option<String> {
        self.content.selection()
    }

    fn select_all(&self, tree: &mut Tree) {
        tree.state.downcast_mut::<EditorWrapperState>().pending_edit = Some(Action::SelectAll);
    }

    fn is_editable(&self) -> bool {
        self.on_action.is_some()
    }

    fn has_text(&self, _tree: &Tree) -> bool {
        !self.content.is_empty()
    }

    fn clipboard_has_text(&self, tree: &Tree) -> bool {
        tree.state
            .downcast_ref::<EditorWrapperState>()
            .clipboard_has_text
    }

    fn is_focused(&self, tree: &Tree) -> bool {
        tree.state.downcast_ref::<EditorWrapperState>().focused
    }

    fn context_menu_position(&self, tree: &Tree) -> Option<Point> {
        tree.state
            .downcast_ref::<EditorWrapperState>()
            .context_menu_position
    }

    fn set_context_menu_position(&self, tree: &mut Tree, pos: Option<Point>) {
        tree.state
            .downcast_mut::<EditorWrapperState>()
            .context_menu_position = pos;
    }

    fn delete_selection(&self, tree: &mut Tree) -> Option<String> {
        tree.state.downcast_mut::<EditorWrapperState>().pending_edit =
            Some(Action::Edit(Edit::Delete));
        Some(String::new())
    }

    fn paste_text(&self, tree: &mut Tree, text: &str) -> Option<String> {
        tree.state.downcast_mut::<EditorWrapperState>().pending_edit = Some(Action::Edit(
            Edit::Paste(std::sync::Arc::new(text.to_owned())),
        ));
        Some(String::new())
    }
}

impl<'a, Message: Clone + 'static> From<TextEditor<'a, Message>> for crate::ui::Element<'a, Message> {
    fn from(editor: TextEditor<'a, Message>) -> Self {
        Self::new(editor)
    }
}
