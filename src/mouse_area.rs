//! A container for capturing mouse events.

use std::time::Instant;

use crate::tab::DOUBLE_CLICK_DURATION;
use crate::ui::convert::{ToColor, ToRadius};
use crate::ui::iced_core::border::Border;
use crate::ui::iced_core::event::Event;
use crate::ui::iced_core::mouse::{self, click};
use crate::ui::iced_core::renderer::{self, Quad, Renderer as _};
use crate::ui::iced_core::widget::{Operation, Tree, tree};
use crate::ui::iced_core::{
    Clipboard, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, layout, overlay,
    touch,
};
use crate::ui::widget::Id;
use crate::ui::{Element, Renderer, Theme};

/// Emit messages on mouse events.
#[allow(missing_debug_implementations)]
pub struct MouseArea<'a, Message> {
    id: Id,
    content: Element<'a, Message>,
    on_auto_scroll: Option<Box<dyn Fn(Option<f32>) -> Message + 'a>>,
    on_drag: Option<Box<dyn Fn(Option<Rectangle>) -> Message + 'a>>,
    on_drag_delta: Option<Box<dyn Fn(Vector) -> Message + 'a>>,
    interaction: Option<mouse::Interaction>,
    on_dnd: Option<Box<dyn Fn(DndDrag) -> Message + 'a>>,
    on_double_click: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_press: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_drag_end: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_release: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_resize: Option<Box<dyn Fn(Rectangle) -> Message + 'a>>,
    on_right_press: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_right_press_no_capture: bool,
    on_right_press_window_position: bool,
    on_right_release: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_middle_press: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_middle_release: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_back_press: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_back_release: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_forward_press: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_forward_release: Option<Box<dyn Fn(Option<Point>) -> Message + 'a>>,
    on_scroll: Option<Box<dyn Fn(mouse::ScrollDelta) -> Option<Message> + 'a>>,
    on_enter: Option<Box<dyn Fn() -> Message + 'a>>,
    on_exit: Option<Box<dyn Fn() -> Message + 'a>>,
    show_drag_rect: bool,
}

impl<'a, Message> MouseArea<'a, Message> {
    /// The message to emit when auto scroll changes.
    #[must_use]
    pub fn on_auto_scroll(mut self, message: impl Fn(Option<f32>) -> Message + 'a) -> Self {
        self.on_auto_scroll = Some(Box::new(message));
        self
    }

    /// The message to emit when a drag is initiated.
    #[must_use]
    pub fn on_drag(mut self, message: impl Fn(Option<Rectangle>) -> Message + 'a) -> Self {
        self.on_drag = Some(Box::new(message));
        self
    }

    /// The cursor to show while the pointer is over this area or a drag
    /// started here is in progress, e.g. a resize cursor on a divider.
    #[must_use]
    pub fn interaction(mut self, interaction: mouse::Interaction) -> Self {
        self.interaction = Some(interaction);
        self
    }

    /// The message to emit while dragging, with the offset of the pointer from
    /// where the drag started. For gestures that move something by an amount,
    /// such as resizing, where the rectangle of [`Self::on_drag`] loses the
    /// direction.
    #[must_use]
    pub fn on_drag_delta(mut self, message: impl Fn(Vector) -> Message + 'a) -> Self {
        self.on_drag_delta = Some(Box::new(message));
        self
    }

    /// The message to emit when a Wayland file drag moves over or is dropped on this
    /// area.
    ///
    /// During a Wayland drag, `wl_data_device` holds the implicit pointer grab and iced
    /// receives no events until the drop. Each `update` polls [`crate::ui::dnd::drag`]
    /// and requests the next redraw to keep polling, using the same clock as tab
    /// dragging.
    #[must_use]
    pub fn on_dnd(mut self, message: impl Fn(DndDrag) -> Message + 'a) -> Self {
        self.on_dnd = Some(Box::new(message));
        self
    }

    /// The message to emit when a drag ends.
    #[must_use]
    pub fn on_drag_end(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_drag_end = Some(Box::new(message));
        self
    }

    /// The message to emit on a double click.
    #[must_use]
    pub fn on_double_click(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_double_click = Some(Box::new(message));
        self
    }

    /// The message to emit on a left button press.
    #[must_use]
    pub fn on_press(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_press = Some(Box::new(message));
        self
    }

    /// The message to emit on a left button release.
    #[must_use]
    pub fn on_release(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_release = Some(Box::new(message));
        self
    }

    /// The message to emit on resizing.
    #[must_use]
    pub fn on_resize(mut self, message: impl Fn(Rectangle) -> Message + 'a) -> Self {
        self.on_resize = Some(Box::new(message));
        self
    }

    /// The message to emit on a right button press.
    #[must_use]
    pub fn on_right_press(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_right_press = Some(Box::new(message));
        self
    }

    /// on_right_press will not capture input
    #[must_use]
    pub fn on_right_press_no_capture(mut self) -> Self {
        self.on_right_press_no_capture = true;
        self
    }

    /// Only on wayland, on_right_press will provide window position instead of widget relative
    #[must_use]
    pub fn wayland_on_right_press_window_position(mut self) -> Self {
        {
            self.on_right_press_window_position = true;
        }
        self
    }

    /// on_right_press will provide window position instead of widget relative
    #[must_use]
    pub fn on_right_press_window_position(mut self) -> Self {
        self.on_right_press_window_position = true;
        self
    }

    /// The message to emit on a right button release.
    #[must_use]
    pub fn on_right_release(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_right_release = Some(Box::new(message));
        self
    }

    /// The message to emit on a middle button press.
    #[must_use]
    pub fn on_middle_press(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_middle_press = Some(Box::new(message));
        self
    }

    /// The message to emit on a middle button release.
    #[must_use]
    pub fn on_middle_release(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_middle_release = Some(Box::new(message));
        self
    }

    /// The message to emit on a back button press.
    #[must_use]
    pub fn on_back_press(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_back_press = Some(Box::new(message));
        self
    }

    /// The message to emit on a back button release.
    #[must_use]
    pub fn on_back_release(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_back_release = Some(Box::new(message));
        self
    }

    /// The message to emit on a forward button press.
    #[must_use]
    pub fn on_forward_press(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_forward_press = Some(Box::new(message));
        self
    }

    /// The message to emit on a forward button release.
    #[must_use]
    pub fn on_forward_release(mut self, message: impl Fn(Option<Point>) -> Message + 'a) -> Self {
        self.on_forward_release = Some(Box::new(message));
        self
    }

    /// The message to emit on a scroll.
    #[must_use]
    pub fn on_scroll(
        mut self,
        message: impl Fn(mouse::ScrollDelta) -> Option<Message> + 'a,
    ) -> Self {
        self.on_scroll = Some(Box::new(message));
        self
    }

    /// The message to emit when a mouse enters the area.
    #[must_use]
    pub fn on_enter(mut self, message: impl Fn() -> Message + 'a) -> Self {
        self.on_enter = Some(Box::new(message));
        self
    }

    /// The message to emit when a mouse exits the area.
    #[must_use]
    pub fn on_exit(mut self, message: impl Fn() -> Message + 'a) -> Self {
        self.on_exit = Some(Box::new(message));
        self
    }

    #[must_use]
    pub const fn show_drag_rect(mut self, show_drag_rect: bool) -> Self {
        self.show_drag_rect = show_drag_rect;
        self
    }

    /// Sets the widget's unique identifier.
    #[must_use]
    pub fn with_id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }
}

/// Where a live Wayland file drag is, in one [`MouseArea`]'s own coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DndDrag {
    /// The drag's position relative to this area's top-left, or `None` when it is over
    /// another pane, window or client. Wayland reports surface-local coordinates, so
    /// inside a scrollable this is measured from the *visible* top-left: a listener
    /// that hit tests against content has to add the scroll offset back.
    pub position: Option<Point>,
    /// The compositor delivered the drop, and `ui::dnd::read_drop` will answer.
    pub dropped: bool,
    /// The drag is over, dropped or not. Exactly one of these arrives per drag,
    /// so it is where a listener undoes whatever the drag was showing.
    pub ended: bool,
}

/// Local state of the [`MouseArea`].
#[derive(Default)]
struct State {
    last_auto_scroll: Option<f32>,
    last_position: Option<Point>,
    last_virtual_position: Option<Point>,
    drag_initiated: Option<Point>,
    /// How far the content has scrolled under the pointer since the press that
    /// started the gesture in progress. See [`State::drag_distance`].
    scrolled_since_press: Vector,
    /// Whether the press that a release would complete happened in this area.
    pressed: bool,
    /// The generation of the Wayland drag this area last reported, so a poll
    /// that saw no change publishes nothing. `None` means no drag is live as
    /// far as this area knows.
    dnd_generation: Option<u64>,
    prev_click: Option<(mouse::Click, Instant)>,
    viewport: Option<Rectangle>,
}

impl State {
    /// Where the cursor is for the purposes of a drag, with the fallback this
    /// has always used for when iced has no cursor to give.
    fn drag_position(&self, cursor: mouse::Cursor) -> Option<Point> {
        cursor.position().or(self.last_virtual_position)
    }

    fn drag_rect(&self, cursor: mouse::Cursor) -> Option<Rectangle> {
        if let Some(drag_source) = self.drag_initiated
            && let Some(position) = self.drag_position(cursor)
            && self.drag_distance(cursor) > 1.0
        {
            let min_x = drag_source.x.min(position.x);
            let max_x = drag_source.x.max(position.x);
            let min_y = drag_source.y.min(position.y);
            let max_y = drag_source.y.max(position.y);
            return Some(Rectangle::new(
                Point::new(min_x, min_y),
                Size::new(max_x - min_x, max_y - min_y),
            ));
        }
        None
    }

    /// How far the pointer physically moved since the press that started the
    /// gesture in progress.
    ///
    /// Not simply the distance from `drag_initiated` to the cursor: an
    /// enclosing scrollable hands its content a cursor shifted by the scroll
    /// offset, so content sliding under a pointer that is standing still moves
    /// the cursor too, and a wheel turn over a pressed item was enough to begin
    /// a file drag. That shift is measured as it happens, in `update`, and
    /// taken back out here, which leaves what the pointer itself did.
    fn drag_distance(&self, cursor: mouse::Cursor) -> f32 {
        let (Some(from), Some(now)) = (self.drag_initiated, self.drag_position(cursor)) else {
            return 0.0;
        };
        (now.x - from.x - self.scrolled_since_press.x)
            .hypot(now.y - from.y - self.scrolled_since_press.y)
    }

    fn click(&mut self, pos: Point) -> mouse::Click {
        let now = Instant::now();

        let new = if let Some((prev_click, prev_time)) = self.prev_click.take() {
            if now.duration_since(prev_time) < DOUBLE_CLICK_DURATION {
                match prev_click.kind() {
                    mouse::click::Kind::Single => {
                        mouse::Click::new(pos, mouse::Button::Left, Some(prev_click))
                    }
                    mouse::click::Kind::Double => {
                        mouse::Click::new(pos, mouse::Button::Left, Some(prev_click))
                    }
                    mouse::click::Kind::Triple => {
                        mouse::Click::new(pos, mouse::Button::Left, Some(prev_click))
                    }
                }
            } else {
                mouse::Click::new(pos, mouse::Button::Left, None)
            }
        } else {
            mouse::Click::new(pos, mouse::Button::Left, None)
        };
        self.prev_click = Some((new, now));
        new
    }
}

impl<'a, Message> MouseArea<'a, Message> {
    /// Creates a [`MouseArea`] with the given content.
    pub fn new(content: impl Into<Element<'a, Message>>) -> Self {
        MouseArea {
            id: Id::unique(),
            content: content.into(),
            on_auto_scroll: None,
            on_drag: None,
            on_drag_delta: None,
            interaction: None,
            on_dnd: None,
            on_drag_end: None,
            on_double_click: None,
            on_press: None,
            on_release: None,
            on_resize: None,
            on_right_press: None,
            on_right_press_no_capture: false,
            on_right_press_window_position: false,
            on_right_release: None,
            on_middle_press: None,
            on_middle_release: None,
            on_back_press: None,
            on_back_release: None,
            on_forward_press: None,
            on_forward_release: None,
            on_enter: None,
            on_exit: None,
            on_scroll: None,
            show_drag_rect: false,
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for MouseArea<'_, Message>
where
    Message: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
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
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if shell.is_event_captured() {
            return;
        }

        update(
            self,
            event,
            layout,
            cursor,
            shell,
            tree.state.downcast_mut::<State>(),
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if let Some(interaction) = self.interaction {
            let dragging = tree.state.downcast_ref::<State>().drag_initiated.is_some();
            if dragging || cursor.is_over(layout.bounds()) {
                return interaction;
            }
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout,
            cursor,
            viewport,
        );

        if self.show_drag_rect {
            let state = tree.state.downcast_ref::<State>();
            if let Some(bounds) = state.drag_rect(cursor) {
                let cosmic = theme.cosmic();
                let mut bg_color = cosmic.accent_color();
                bg_color.alpha = 0.2;
                renderer.start_layer(*viewport);
                renderer.fill_quad(
                    Quad {
                        bounds,
                        border: Border {
                            color: cosmic.accent_color().to_color(),
                            width: 1.0,
                            radius: cosmic.radius_xs().to_radius(),
                        },
                        ..Default::default()
                    },
                    bg_color.to_color(),
                );
                renderer.end_layer();
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message> From<MouseArea<'a, Message>> for Element<'a, Message>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer,
    Theme: 'a,
{
    fn from(area: MouseArea<'a, Message>) -> Self {
        Element::new(area)
    }
}

/// Processes the given [`Event`] and updates the [`State`] of an [`MouseArea`]
/// accordingly.
fn update<Message: Clone>(
    widget: &mut MouseArea<'_, Message>,
    event: &Event,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
    shell: &mut Shell<'_, Message>,
    state: &mut State,
    viewport: &Rectangle,
) {
    // No virtual offset: this fed only `on_right_press_window_position`, which
    // nothing in this app enables.
    let offset = iced_core::Vector::ZERO;
    let layout_bounds = layout.bounds();

    // Poll Wayland file drags before handling hover and cursor events; see
    // `MouseArea::on_dnd`. Iced has no cursor while the data device holds the pointer
    // grab.
    if let Some(on_dnd) = widget.on_dnd.as_ref() {
        match crate::ui::dnd::drag() {
            // Tab drags use the same `wl_data_device` but carry no file list, so this
            // area ignores them.
            Some(drag) if drag.files => {
                if !drag.ended {
                    shell.request_redraw();
                }
                if state.dnd_generation != Some(drag.generation) {
                    state.dnd_generation = Some(drag.generation);
                    let position = drag
                        .position
                        .map(|(x, y)| Point::new(x, y))
                        .filter(|point| layout_bounds.contains(*point))
                        .map(|point| point - Vector::new(layout_bounds.x, layout_bounds.y));
                    shell.publish(on_dnd(DndDrag {
                        position,
                        dropped: drag.dropped,
                        ended: drag.ended,
                    }));
                }
            }
            // Ignore tab drags. Treating one as the end of a file drag would make
            // `Tab::end_file_drag` call `ui::dnd::end_drag` on the new tab drag,
            // preventing tab reordering. The arm above handles ended file drags, which
            // remain readable until the next drag starts.
            Some(_) => state.dnd_generation = None,
            // No drag is available: initialization failed or `ui::dnd` lost the drag.
            // Report its end once to clear the hint, since no further end notification
            // will arrive.
            None => {
                if state.dnd_generation.take().is_some() {
                    shell.publish(on_dnd(DndDrag {
                        position: None,
                        dropped: false,
                        ended: true,
                    }));
                }
            }
        }
    }

    // A scrollable hands its content a viewport of the part that is on screen,
    // in the same coordinates it shifts the cursor into, so this origin moves
    // by exactly the scroll offset — on every event, however deeply nested this
    // area is. Read from here rather than inferred from the kind of event: the
    // offset can first reach this area on a pointer movement, when a wheel turn
    // and a motion arrive in one batch, and the scrolling would then be counted
    // as the pointer having moved.
    //
    // Only while the area keeps its size, because that origin also moves when
    // the window or a pane is resized, which is not the content scrolling.
    if let Some(previous) = state.viewport
        && previous.size() == viewport.size()
    {
        state.scrolled_since_press += viewport.position() - previous.position();
    }

    let viewport_changed = state.viewport != Some(*viewport);

    if let Some(message) = widget.on_resize.as_ref()
        && viewport_changed
    {
        shell.publish(message(*viewport));
    }

    state.viewport = Some(*viewport);

    let should_check_hover = viewport_changed
        || matches!(
            event,
            Event::Mouse(mouse::Event::CursorMoved { .. })
                | Event::Mouse(mouse::Event::WheelScrolled { .. })
        );

    if should_check_hover {
        let position_in = cursor.position_in(layout_bounds);
        match (position_in, state.last_position) {
            (None, Some(_)) => {
                if let Some(message) = widget.on_exit.as_ref() {
                    shell.publish(message());
                }
            }
            (Some(_), None) => {
                if let Some(message) = widget.on_enter.as_ref() {
                    shell.publish(message());
                }
            }
            _ => {}
        }
        state.last_position = position_in;
    }

    if state.drag_initiated.is_some()
        && matches!(
            event,
            Event::Mouse(mouse::Event::CursorMoved { .. })
                | Event::Touch(touch::Event::FingerMoved { .. })
        )
    {
        shell.request_redraw();
    }

    if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
        let virtual_position = Point::new(
            viewport.x - layout_bounds.x + position.x,
            viewport.y - layout_bounds.y + position.y,
        );
        state.last_virtual_position = Some(virtual_position);

        if let Some(message) = widget.on_auto_scroll.as_ref() {
            let auto_scroll = if state.drag_initiated.is_some() {
                let bottom = viewport.y;
                let top = viewport.y + viewport.height;
                if virtual_position.y < bottom {
                    Some(virtual_position.y - bottom)
                } else if virtual_position.y > top {
                    Some(virtual_position.y - top)
                } else {
                    None
                }
            } else {
                None
            };
            if state.last_auto_scroll != auto_scroll {
                shell.publish(message(auto_scroll));
                state.last_auto_scroll = auto_scroll;
            }
        }
    }

    // Whether the press this release completes happened in this area. Retained
    // double-click history is not evidence of that: an item clicked a moment
    // ago would otherwise claim the release of a rubber-band selection that
    // began on the background, capture it, and leave that gesture never told it
    // had ended. Taken on every release, wherever the pointer is, so a gesture
    // that ends elsewhere does not leave the flag set behind it.
    let released = matches!(
        event,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
    );
    let pressed_here = if released {
        std::mem::take(&mut state.pressed)
    } else {
        state.pressed
    };

    if state.drag_initiated.is_none() && !cursor.is_over(layout_bounds) {
        return;
    }

    if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    | Event::Touch(touch::Event::FingerPressed { .. }) = event
    {
        let click = state.click(cursor.position_in(layout_bounds).unwrap_or_default());
        match click.kind() {
            click::Kind::Single => {
                if let Some(message) = widget.on_press.as_ref() {
                    shell.publish(message(cursor.position_in(layout_bounds)));
                }
            }
            click::Kind::Double => {
                if let Some(message) = widget.on_double_click.as_ref() {
                    shell.publish(message(cursor.position_in(layout_bounds)));
                }
            }
            click::Kind::Triple => {
                if let Some(message) = widget.on_press.as_ref() {
                    shell.publish(message(cursor.position_in(layout_bounds)));
                }
            }
        }
        state.pressed = true;
        if widget.on_drag.is_some() || widget.on_drag_delta.is_some() {
            state.drag_initiated = cursor.position();
            state.scrolled_since_press = Vector::ZERO;
        }

        if widget.on_press.is_some() {
            shell.capture_event();
            return;
        }
    }

    let distance_dragged = state.drag_distance(cursor);
    if released && distance_dragged > 1.0 {
        state.drag_initiated = None;
        state.prev_click = None;
        shell.request_redraw();
        if let Some(message) = widget.on_drag_end.as_ref() {
            shell.publish(message(cursor.position_in(layout_bounds)));
        }
    } else if matches!(event, Event::Mouse(mouse::Event::CursorLeft)) {
        // End the gesture when the compositor takes the pointer grab; no button release
        // will arrive here.
        //
        // The header bar's `on_drag` requests an interactive move
        // (`xdg_toplevel.move`). The compositor takes the grab and sends
        // `wl_pointer.leave`. Without this arm, `drag_initiated` stays set and
        // `drag_rect` falls back to `last_virtual_position`. Each subsequent event then
        // republishes `on_drag` and requests another redraw. A single header drag
        // caused about 55 xdg_toplevel.move requests per second and 50-85% CPU usage
        // until the app was killed.
        //
        // Keep `on_drag_end` release-only. Losing the pointer does not complete the
        // gesture, and rubber-band selection should retain its published state without
        // committing on leave. A held button normally keeps an implicit grab on the
        // surface, so this arm handles a stolen grab, not ordinary dragging.
        state.drag_initiated = None;
        state.prev_click = None;
        state.pressed = false;
    }

    let recent_click = state
        .prev_click
        .as_ref()
        .is_some_and(|(_, i)| Instant::now().duration_since(*i) <= DOUBLE_CLICK_DURATION);
    if released && pressed_here && state.prev_click.is_some() {
        if !recent_click {
            state.prev_click = None;
            // A press held without moving is over too: otherwise later pointer
            // movement would still report a drag
            state.drag_initiated = None;
            return;
        }
        if state.drag_initiated.take().is_some() {
            shell.request_redraw();
        }
        if let Some(message) = widget.on_release.as_ref() {
            shell.publish(message(cursor.position_in(layout_bounds)));

            shell.capture_event();
            return;
        }
    }

    if let Some(message) = widget.on_right_press.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
        )
    {
        let point_opt = if widget.on_right_press_window_position {
            cursor.position_over(layout_bounds).map(|mut p| {
                p.x -= offset.x;
                p.y -= offset.y;
                p
            })
        } else {
            cursor.position_in(layout_bounds)
        };
        shell.publish(message(point_opt));

        if widget.on_right_press_no_capture {
            return;
        }
        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_right_release.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_middle_press.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_middle_release.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Middle))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_back_press.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Back))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_back_release.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Back))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_forward_press.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Forward))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(message) = widget.on_forward_release.as_ref()
        && matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Forward))
        )
    {
        shell.publish(message(cursor.position_in(layout_bounds)));

        shell.capture_event();
        return;
    }

    if let Some(on_scroll) = widget.on_scroll.as_ref()
        && let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
        && let Some(message) = on_scroll(*delta)
    {
        shell.publish(message);
        shell.capture_event();
        return;
    }

    if let Some((message, drag_rect)) = widget.on_drag.as_ref().zip(state.drag_rect(cursor)) {
        shell.publish(message(drag_rect.intersection(&layout_bounds).map(
            |mut rect| {
                rect.x -= layout_bounds.x;
                rect.y -= layout_bounds.y;
                rect
            },
        )));
    }

    if let Some(message) = widget.on_drag_delta.as_ref()
        && matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. }))
        && let Some(source) = state.drag_initiated
        && let Some(position) = cursor.position()
    {
        shell.publish(message(position - source));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{MouseArea, State, update};
    use crate::ui::iced_core::event::Event;
    use crate::ui::iced_core::mouse::{self, Cursor};
    use crate::ui::iced_core::{Layout, Point, Rectangle, Shell, Size, layout, window};
    use crate::ui::widget::space;

    #[derive(Clone, Debug, PartialEq)]
    enum Msg {
        Drag(Option<Rectangle>),
        Release,
    }

    const AREA: Size = Size::new(200.0, 200.0);

    /// Hand one event to `update`, and report what it published.
    ///
    /// `pointer` is where the pointer is in the window. A scrollable shifts
    /// both of the things it hands its content by the scroll offset — the
    /// cursor and the viewport — so `scrolled` moves the two together, as one.
    fn feed_scrolled(
        widget: &mut MouseArea<'_, Msg>,
        state: &mut State,
        event: &Event,
        pointer: Point,
        scrolled: f32,
    ) -> Vec<Msg> {
        let node = layout::Node::new(AREA);
        let layout = Layout::new(&node);
        let viewport = Rectangle::new(Point::new(0.0, scrolled), AREA);
        let cursor = Cursor::Available(Point::new(pointer.x, pointer.y + scrolled));
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        update(widget, event, layout, cursor, &mut shell, state, &viewport);
        messages
    }

    /// The same, for an area nothing has scrolled.
    fn feed(
        widget: &mut MouseArea<'_, Msg>,
        state: &mut State,
        event: &Event,
        pointer: Point,
    ) -> Vec<Msg> {
        feed_scrolled(widget, state, event, pointer, 0.0)
    }

    fn moved_to(position: Point) -> Event {
        Event::Mouse(mouse::Event::CursorMoved { position })
    }

    const fn pressed() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    const fn released() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    /// An enclosing scrollable hands its content a cursor shifted by the scroll
    /// offset, so scrolling with the button held moves the cursor although the
    /// pointer has not: a wheel turn over a pressed item must not become a drag.
    #[test]
    fn scrolling_under_a_held_button_is_not_a_drag() {
        let mut widget = MouseArea::new(space::vertical()).on_drag(Msg::Drag);
        let mut state = State::default();

        let at = Point::new(10.0, 10.0);
        feed(&mut widget, &mut state, &moved_to(at), at);
        feed(&mut widget, &mut state, &pressed(), at);

        // 100 pixels of scrolling, and the redraw that follows it
        let published = feed_scrolled(
            &mut widget,
            &mut state,
            &Event::Window(window::Event::RedrawRequested(Instant::now())),
            at,
            100.0,
        );
        assert!(
            published.is_empty(),
            "the pointer never moved, so nothing was dragged: {published:?}"
        );

        // The pointer itself moving still is a drag, scrolled or not
        let published = feed_scrolled(
            &mut widget,
            &mut state,
            &moved_to(Point::new(60.0, 60.0)),
            Point::new(60.0, 60.0),
            100.0,
        );
        assert!(
            matches!(published.as_slice(), [Msg::Drag(Some(_))]),
            "a pointer that moved drags: {published:?}"
        );
    }

    /// An item that scrolls into view under a stationary pointer is a brand
    /// new widget with brand new state: it never saw a pointer move, so there
    /// is no window position to compare against. A press on it, followed by
    /// more scrolling, must still not be a drag.
    #[test]
    fn a_press_on_a_freshly_built_area_is_not_dragged_by_scrolling() {
        let mut widget = MouseArea::new(space::vertical()).on_drag(Msg::Drag);
        // Default: this area has never seen a pointer event
        let mut state = State::default();

        let at = Point::new(10.0, 10.0);
        feed(&mut widget, &mut state, &pressed(), at);

        let published = feed_scrolled(
            &mut widget,
            &mut state,
            &Event::Window(window::Event::RedrawRequested(Instant::now())),
            at,
            100.0,
        );
        assert!(
            published.is_empty(),
            "the pointer never moved, so nothing was dragged: {published:?}"
        );

        // Once the pointer does move, it drags as usual
        feed_scrolled(&mut widget, &mut state, &moved_to(at), at, 100.0);
        let published = feed_scrolled(
            &mut widget,
            &mut state,
            &moved_to(Point::new(60.0, 60.0)),
            Point::new(60.0, 60.0),
            100.0,
        );
        assert!(
            matches!(published.as_slice(), [Msg::Drag(Some(_))]),
            "a pointer that moved drags: {published:?}"
        );
    }

    /// The first pointer movement of a gesture can be a long one — a flick, or
    /// several motions the compositor delivered as one — and it counts. An area
    /// that has only just been built must not swallow it and read the gesture
    /// as a click, which in single-click mode opens the item instead.
    #[test]
    fn the_first_movement_of_a_drag_counts() {
        let mut widget = MouseArea::new(space::vertical()).on_drag(Msg::Drag);
        let mut state = State::default();

        feed(&mut widget, &mut state, &pressed(), Point::new(10.0, 10.0));
        let far = Point::new(100.0, 100.0);
        let published = feed(&mut widget, &mut state, &moved_to(far), far);
        assert!(
            matches!(published.as_slice(), [Msg::Drag(Some(_))]),
            "one movement of 127 pixels is a drag: {published:?}"
        );
    }

    /// A wheel turn and a pointer movement can arrive in one batch of events.
    /// The scrollable applies the new offset after handing the wheel event
    /// down, so the offset first reaches this area on the movement itself —
    /// and that scrolling is still not the pointer moving.
    #[test]
    fn scrolling_in_the_same_batch_as_a_movement_is_not_a_drag() {
        let mut widget = MouseArea::new(space::vertical()).on_drag(Msg::Drag);
        let mut state = State::default();

        let at = Point::new(10.0, 10.0);
        feed(&mut widget, &mut state, &moved_to(at), at);
        feed(&mut widget, &mut state, &pressed(), at);

        // The wheel reaches this area before the offset it causes does
        feed(
            &mut widget,
            &mut state,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            at,
        );
        // The movement that follows carries 100 pixels of scrolling and half a
        // pixel of pointer travel, with no redraw in between to tell them apart
        let nudged = Point::new(10.0, 10.5);
        let published = feed_scrolled(&mut widget, &mut state, &moved_to(nudged), nudged, 100.0);
        assert!(
            published.is_empty(),
            "half a pixel of travel is not a drag, whatever scrolled with it: {published:?}"
        );
    }

    /// A release belongs to the area that saw the press it completes. Retained
    /// double-click history is not evidence of that: an item clicked a moment
    /// ago would otherwise claim, and capture, the release of a gesture that
    /// began on the background behind it.
    #[test]
    fn a_release_needs_the_press_that_started_it() {
        let mut widget = MouseArea::new(space::vertical()).on_release(|_| Msg::Release);
        let mut state = State::default();

        let at = Point::new(10.0, 10.0);
        feed(&mut widget, &mut state, &moved_to(at), at);
        feed(&mut widget, &mut state, &pressed(), at);
        assert_eq!(
            feed(&mut widget, &mut state, &released(), at),
            vec![Msg::Release],
            "its own click ends in a release"
        );

        // A second release over the same area, from a gesture that began
        // somewhere else, inside the double-click interval
        let published = feed(&mut widget, &mut state, &released(), at);
        assert!(
            published.is_empty(),
            "a release with no press of its own is not this area's: {published:?}"
        );
    }
}
