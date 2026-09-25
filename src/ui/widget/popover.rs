// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/popover.rs

//! A container which displays an overlay when a popup widget is attached.

use iced::widget;
use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, layout,
    mouse, overlay, renderer, touch,
};

pub use iced::widget::container::{Catalog, Style};

pub fn popover<'a, Message, Renderer>(
    content: impl Into<Element<'a, Message, crate::ui::Theme, Renderer>>,
) -> Popover<'a, Message, Renderer> {
    Popover::new(content)
}

#[derive(Clone, Copy, Debug, Default)]
pub enum Position {
    #[default]
    Center,
    Bottom,
    Top,
    Point(Point),
}

/// A container which displays overlays when a popup widget is assigned.
#[must_use]
pub struct Popover<'a, Message, Renderer> {
    id: widget::Id,
    content: Element<'a, Message, crate::ui::Theme, Renderer>,
    modal: bool,
    popup: Option<Element<'a, Message, crate::ui::Theme, Renderer>>,
    position: Position,
    on_close: Option<Message>,
}

impl<'a, Message, Renderer> Popover<'a, Message, Renderer> {
    pub fn new(content: impl Into<Element<'a, Message, crate::ui::Theme, Renderer>>) -> Self {
        Self {
            id: widget::Id::unique(),
            content: content.into(),
            modal: false,
            popup: None,
            position: Position::Center,
            on_close: None,
        }
    }

    /// Set the Id
    #[inline]
    pub fn id(mut self, id: widget::Id) -> Self {
        self.id = id;
        self
    }

    /// A modal popup intercepts user inputs while a popup is active.
    #[inline]
    pub fn modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }

    /// Emitted when the popup is closed.
    #[inline]
    pub fn on_close(mut self, on_close: Message) -> Self {
        self.on_close = Some(on_close);
        self
    }

    #[inline]
    pub fn popup(
        mut self,
        popup: impl Into<Element<'a, Message, crate::ui::Theme, Renderer>>,
    ) -> Self {
        self.popup = Some(popup.into());
        self
    }

    #[inline]
    pub fn position(mut self, position: Position) -> Self {
        self.position = position;
        self
    }
}

impl<Message: Clone, Renderer> Widget<Message, crate::ui::Theme, Renderer>
    for Popover<'_, Message, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        if let Some(popup) = &self.popup {
            vec![Tree::new(&self.content), Tree::new(popup)]
        } else {
            vec![Tree::new(&self.content)]
        }
    }

    fn diff(&self, tree: &mut Tree) {
        if let Some(popup) = &self.popup {
            tree.diff_children(&[&self.content, popup]);
        } else {
            tree.diff_children(&[&self.content]);
        }
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let tree = &mut tree.children[0];
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Skip operating on background content, prevents Tab from escaping
        if self.modal && self.popup.is_some() {
            return;
        }
        self.content
            .as_widget_mut()
            .operate(content_tree_mut(tree), layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.popup.is_some() {
            if self.modal {
                if matches!(event, Event::Mouse(_) | Event::Touch(_)) {
                    // A dialog can appear while a button is held on the
                    // content (a double click that opens one), and the content
                    // must still see that press end: a file item would
                    // otherwise start a drag on the first move after the dialog
                    // closes. It gets no cursor, and what it publishes is
                    // dropped, because a release is not inert even then (a
                    // mouse area's `on_release` does not look at the cursor).
                    if ends_gesture(event) {
                        let mut discarded = Vec::new();
                        let mut content_shell = Shell::new(&mut discarded);
                        self.content.as_widget_mut().update(
                            content_tree_mut(tree),
                            event,
                            layout,
                            mouse::Cursor::Unavailable,
                            renderer,
                            clipboard,
                            &mut content_shell,
                            viewport,
                        );
                        if content_shell.is_layout_invalid() {
                            shell.invalidate_layout();
                        }
                        if content_shell.are_widgets_invalid() {
                            shell.invalidate_widgets();
                        }
                        shell.request_redraw_at(content_shell.redraw_request());
                        return;
                    }
                    shell.capture_event();
                    return;
                }
            } else if let Some(on_close) = self.on_close.as_ref()
                && matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(_))
                        | Event::Touch(touch::Event::FingerPressed { .. })
                )
                && !cursor_position.is_over(layout.bounds())
            {
                shell.publish(on_close.clone());
            }
        }

        // Hide cursor from background content when modal popup is active
        let cursor = if self.modal && self.popup.is_some() {
            mouse::Cursor::Unavailable
        } else {
            cursor_position
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.modal && self.popup.is_some() && cursor_position.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        self.content.as_widget().mouse_interaction(
            content_tree(tree),
            layout,
            cursor_position,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &crate::ui::Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // Hide cursor from background content when a modal popup is active
        let cursor = if self.modal && self.popup.is_some() {
            mouse::Cursor::Unavailable
        } else {
            cursor_position
        };
        self.content.as_widget().draw(
            content_tree(tree),
            renderer,
            theme,
            renderer_style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        mut translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, Renderer>> {
        if let Some(popup) = &mut self.popup {
            let bounds = layout.bounds();

            // Calculate overlay position from relative position
            let mut overlay_position = match self.position {
                Position::Center => Point::new(
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                ),
                Position::Bottom => {
                    Point::new(bounds.x + bounds.width / 2.0, bounds.y + bounds.height)
                }
                Position::Point(relative) => {
                    bounds.position() + Vector::new(relative.x, relative.y)
                }
                Position::Top => Point::new(bounds.x + bounds.width / 2.0, bounds.y),
            };

            // Round position to prevent rendering issues
            overlay_position.x = overlay_position.x.round();
            overlay_position.y = overlay_position.y.round();
            translation.x += overlay_position.x;
            translation.y += overlay_position.y;
            Some(overlay::Element::new(Box::new(Overlay {
                tree: &mut tree.children[1],
                content: popup,
                position: self.position,
                pos: Point::new(translation.x, translation.y),
                modal: self.modal,
            })))
        } else {
            self.content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            )
        }
    }
}

impl<'a, Message, Renderer> From<Popover<'a, Message, Renderer>>
    for Element<'a, Message, crate::ui::Theme, Renderer>
where
    Message: 'static + Clone,
    Renderer: iced_core::Renderer + 'static,
{
    fn from(popover: Popover<'a, Message, Renderer>) -> Self {
        Self::new(popover)
    }
}

pub struct Overlay<'a, 'b, Message, Renderer> {
    tree: &'a mut Tree,
    content: &'a mut Element<'b, Message, crate::ui::Theme, Renderer>,
    position: Position,
    pos: Point,
    modal: bool,
}

impl<Message, Renderer> overlay::Overlay<Message, crate::ui::Theme, Renderer>
    for Overlay<'_, '_, Message, Renderer>
where
    Message: Clone,
    Renderer: iced_core::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let mut position = self.pos;
        let limits = layout::Limits::new(Size::UNIT, bounds);
        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        match self.position {
            Position::Center => {
                // Position is set to the center of the widget
                let width = node.size().width;
                let height = node.size().height;
                position.x = (position.x - width / 2.0).clamp(0.0, bounds.width - width);
                position.y = (position.y - height / 2.0).clamp(0.0, bounds.height - height);
            }
            Position::Top => {
                let width = node.size().width;
                let height = node.size().height;
                position.x = (position.x - width / 2.0).clamp(0.0, bounds.width - width);
                position.y = (position.y - height).clamp(0.0, bounds.height - height);
            }
            Position::Bottom => {
                // Position is set to the center bottom of the widget
                let width = node.size().width;
                let height = node.size().height;
                position.x = (position.x - width / 2.0).clamp(0.0, bounds.width - width);
                position.y = position.y.clamp(0.0, bounds.height - height);
            }
            Position::Point(_) => {
                // Position is using context menu logic
                let size = node.size();
                position.x = position.x.clamp(0.0, bounds.width - size.width);
                if position.y + size.height > bounds.height {
                    position.y = (position.y - size.height).clamp(0.0, bounds.height - size.height);
                }
            }
        }

        // Round position to prevent rendering issues
        position.x = position.x.round();
        position.y = position.y.round();

        node.move_to(position)
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        if self.modal
            && matches!(event, Event::Mouse(_) | Event::Touch(_))
            && !cursor_position.is_over(layout.bounds())
        {
            // Swallow new presses outside the popup, but still forward other
            // events so an interaction started inside it (such as a selection
            // drag) receives its release once the cursor leaves the bounds.
            let is_press = matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            );
            if !is_press {
                self.content.as_widget_mut().update(
                    self.tree,
                    event,
                    layout,
                    cursor_position,
                    renderer,
                    clipboard,
                    shell,
                    &layout.bounds(),
                );
            }
            // Unless the popup took it, the end of a gesture goes on to the
            // content beneath, which may have seen the press before the popup
            // appeared; see `Popover::update`.
            if !ends_gesture(event) {
                shell.capture_event();
            }
            return;
        }

        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor_position,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        )
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.modal && !cursor_position.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }

        self.content.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor_position,
            &layout.bounds(),
            renderer,
        )
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        self.content.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor_position,
            &bounds,
        );
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, Message, crate::ui::Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            self.tree,
            layout,
            renderer,
            &layout.bounds(),
            Default::default(),
        )
    }
}

/// Whether `event` ends a press or touch, however it ends: released, lost, or
/// with the pointer gone from the window.
fn ends_gesture(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(_) | mouse::Event::CursorLeft)
            | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. })
    )
}

/// The local state of a [`Popover`].
#[derive(Debug, Default)]
struct State {
    is_open: bool,
}

/// The first child in [`Popover::children`] is always the wrapped content.
fn content_tree(tree: &Tree) -> &Tree {
    &tree.children[0]
}

/// The first child in [`Popover::children`] is always the wrapped content.
fn content_tree_mut(tree: &mut Tree) -> &mut Tree {
    &mut tree.children[0]
}

#[cfg(test)]
mod tests {
    use iced_core::clipboard;
    use iced_runtime::user_interface::{Cache, UserInterface};

    use super::*;

    const WINDOW: Size = Size::new(200.0, 200.0);
    /// On the item, and clear of the 20×20 dialog centred in the window.
    const ON_ITEM: Point = Point::new(50.0, 50.0);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Msg {
        Pressed,
        Released,
        /// What a file item publishes as it starts a file drag.
        Dragged,
    }

    /// A 100×100 item that can be dragged, beneath a modal popover that shows
    /// a dialog when `dialog` is set.
    fn view(dialog: bool) -> crate::ui::Element<'static, Msg> {
        let item = crate::mouse_area::MouseArea::new(
            crate::ui::widget::space::horizontal()
                .width(Length::Fixed(100.0))
                .height(Length::Fixed(100.0)),
        )
        .on_press(|_| Msg::Pressed)
        .on_release(|_| Msg::Released)
        .on_drag(|_| Msg::Dragged);
        let popover = popover(
            iced::widget::container(item)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .modal(true);
        if dialog {
            popover
                .popup(
                    crate::ui::widget::space::horizontal()
                        .width(Length::Fixed(20.0))
                        .height(Length::Fixed(20.0)),
                )
                .into()
        } else {
            popover.into()
        }
    }

    struct Harness {
        renderer: crate::ui::Renderer,
        cache: Option<Cache>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
            }
        }

        /// Builds the view afresh, as the runtime does after messages, and
        /// hands it `event` with the pointer at `cursor`.
        fn send(&mut self, dialog: bool, cursor: Point, event: Event) -> Vec<Msg> {
            let mut messages = Vec::new();
            let mut ui = UserInterface::build(
                view(dialog),
                WINDOW,
                self.cache.take().unwrap(),
                &mut self.renderer,
            );
            let _ = ui.update(
                &[event],
                mouse::Cursor::Available(cursor),
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = Some(ui.into_cache());
            messages
        }
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    fn moved(position: Point) -> Event {
        Event::Mouse(mouse::Event::CursorMoved { position })
    }

    /// The press lands on the item, then a dialog opens before the release
    /// (as one opened by a double click does).
    fn press_then_open_dialog(ui: &mut Harness) {
        assert_eq!(ui.send(false, ON_ITEM, press()), [Msg::Pressed]);
    }

    #[test]
    fn a_release_under_a_dialog_ends_the_press_beneath_it() {
        let mut ui = Harness::new();
        press_then_open_dialog(&mut ui);
        ui.send(true, ON_ITEM, release());

        // The dialog closes and the pointer moves on: no drag starts.
        let away = Point::new(80.0, 80.0);
        assert_eq!(ui.send(false, away, moved(away)), []);
    }

    #[test]
    fn a_release_over_the_dialog_ends_the_press_beneath_it() {
        let mut ui = Harness::new();
        press_then_open_dialog(&mut ui);
        let on_dialog = Point::new(100.0, 100.0);
        ui.send(true, on_dialog, release());

        let away = Point::new(80.0, 80.0);
        assert_eq!(ui.send(false, away, moved(away)), []);
    }

    #[test]
    fn a_release_under_a_dialog_activates_nothing_beneath_it() {
        let mut ui = Harness::new();
        press_then_open_dialog(&mut ui);

        assert_eq!(ui.send(true, ON_ITEM, release()), []);
    }

    #[test]
    fn a_press_under_a_dialog_still_reaches_nothing_beneath_it() {
        let mut ui = Harness::new();

        assert_eq!(ui.send(true, ON_ITEM, press()), []);
        assert_eq!(ui.send(true, ON_ITEM, release()), []);
    }
}
