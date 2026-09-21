// Copyright 2019 Héctor Ramón, Iced contributors
// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: MIT

//! A selectable, focusable text widget.
//!
//! Vendored from pop-os/libcosmic, iced/core/src/widget/text.rs.
//!
//! Provides click-and-drag selection, focus, keyboard navigation, Ctrl+C /
//! Ctrl+A and the right-click context-menu hooks that
//! `ui::widget::selectable_text` delegates to. `src/tab.rs` uses this widget
//! in the details pane to let users select and copy a filename, path or
//! timestamp.
//!
//!   * The `Widget` impl, and everything that constructs one, is concrete in
//!     `crate::ui::Renderer`: the selection highlight needs `Paragraph::highlight`,
//!     which [`crate::ui::widget::paragraph`] provides as a free function over
//!     the concrete `iced_graphics::text::Paragraph`.
//!   * There is no `ellipsize` here; this crate ellipsizes with the standalone
//!     [`crate::ui::widget::ellipsize::Ellipsize`] widget instead.
//!   * `Style` carries `selected_fill`/`selected_text_color`, so this module
//!     has its own `Style` and `Catalog`. `crate::ui::Theme` implements this
//!     `Catalog` in `ui::theme::style::iced`.
//!   * `text::Affinity` is `cosmic_text::Affinity` (see
//!     [`crate::ui::widget::paragraph`]).

use cosmic_text::Affinity;
use iced_core::alignment;
use iced_core::layout;
use iced_core::mouse::{self, click};
use iced_core::renderer;
use iced_core::text;
use iced_core::text::paragraph::{self, Paragraph};
use iced_core::widget::tree::{self, Tree};
// `fill_quad`/`with_layer` come from `iced_core::Renderer` and `fill_paragraph`
// from `text::Renderer`. Both imported anonymously so neither collides with the
// concrete `Renderer` alias below.
use iced_core::Renderer as _;
use iced_core::text::Renderer as _;
use iced_core::{
    Clipboard, Color, Element, Event, Layout, Length, Pixels, Point, Rectangle, Shell, Size,
    Widget, keyboard, touch,
};

use unicode_segmentation::UnicodeSegmentation;

pub use text::{Alignment, LineHeight, Shaping, Wrapping};

/// A bunch of text.
///
/// # Example
/// ```no_run
/// # mod iced { pub mod widget { pub fn text<T>(t: T) -> iced_core::widget::Text<'static, iced_core::Theme, ()> { unimplemented!() } }
/// #            pub use iced_core::color; }
/// # pub type State = ();
/// # pub type Element<'a, Message> = iced_core::Element<'a, Message, iced_core::Theme, ()>;
/// use iced::widget::text;
/// use iced::color;
///
/// enum Message {
///     // ...
/// }
///
/// fn view(state: &State) -> Element<'_, Message> {
///     text("Hello, this is iced!")
///         .size(20)
///         .color(color!(0x0000ff))
///         .into()
/// }
/// ```
pub struct Text<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fragment: text::Fragment<'a>,
    format: Format<Renderer::Font>,
    class: Theme::Class<'a>,
    selectable: bool,
}

impl<'a, Theme, Renderer> Text<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Create a new fragment of [`Text`] with the given contents.
    pub fn new(fragment: impl text::IntoFragment<'a>) -> Self {
        Text {
            fragment: fragment.into_fragment(),
            format: Format::default(),
            class: Theme::default(),
            selectable: false,
        }
    }

    /// Sets the size of the [`Text`].
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.format.size = Some(size.into());
        self
    }

    /// Sets the [`LineHeight`] of the [`Text`].
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.format.line_height = line_height.into();
        self
    }

    /// Sets the [`Font`] of the [`Text`].
    ///
    /// [`Font`]: crate::text::Renderer::Font
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.format.font = Some(font.into());
        self
    }

    /// Sets the [`Font`] of the [`Text`], if `Some`.
    ///
    /// [`Font`]: crate::text::Renderer::Font
    pub fn font_maybe(mut self, font: Option<impl Into<Renderer::Font>>) -> Self {
        self.format.font = font.map(Into::into);
        self
    }

    /// Sets the width of the [`Text`] boundaries.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.format.width = width.into();
        self
    }

    /// Sets the height of the [`Text`] boundaries.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.format.height = height.into();
        self
    }

    /// Centers the [`Text`], both horizontally and vertically.
    pub fn center(self) -> Self {
        self.align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
    }

    /// Sets the [`alignment::Horizontal`] of the [`Text`].
    pub fn align_x(mut self, alignment: impl Into<text::Alignment>) -> Self {
        self.format.align_x = alignment.into();
        self
    }

    /// Sets the [`alignment::Vertical`] of the [`Text`].
    pub fn align_y(mut self, alignment: impl Into<alignment::Vertical>) -> Self {
        self.format.align_y = alignment.into();
        self
    }

    /// Sets the [`Shaping`] strategy of the [`Text`].
    pub fn shaping(mut self, shaping: Shaping) -> Self {
        self.format.shaping = shaping;
        self
    }

    /// Sets the [`Wrapping`] strategy of the [`Text`].
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.format.wrapping = wrapping;
        self
    }

    /// Sets the style of the [`Text`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the [`Color`] of the [`Text`].
    pub fn color(self, color: impl Into<Color>) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.color_maybe(Some(color))
    }

    /// Sets the [`Color`] of the [`Text`], if `Some`.
    pub fn color_maybe(self, color: Option<impl Into<Color>>) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        let color = color.map(Into::into);

        self.style(move |_theme| Style {
            color,
            ..Style::default()
        })
    }

    /// Makes the [`Text`] selectable. When enabled, the user can click and
    /// drag to select text, and copy it with Ctrl+C / Cmd+C.
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// Sets the style class of the [`Text`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

/// The internal state of a [`Text`] widget.
// The default type parameter is this crate's addition: the `Widget` impl is
// concrete in `crate::ui::Renderer`, so the tree state always has this paragraph,
// and the parent module can then name it as plain `widget::State`.
pub struct State<P: Paragraph = crate::ui::widget::paragraph::Paragraph> {
    /// The cached paragraph layout.
    pub paragraph: paragraph::Plain<P>,
    /// Lazily allocated when text is selectable and first interacted with.
    selection: Option<Box<SelectionState>>,
    focused: bool,
    keyboard_focused: bool,
    context_menu_position: Option<Point>,
    clipboard_has_text: bool,
}

impl<P: Paragraph> Default for State<P> {
    fn default() -> Self {
        Self {
            paragraph: paragraph::Plain::default(),
            selection: None,
            focused: false,
            keyboard_focused: false,
            context_menu_position: None,
            clipboard_has_text: false,
        }
    }
}

impl<P: Paragraph> std::fmt::Debug for State<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("selection", &self.selection)
            .field("focused", &self.focused)
            .finish_non_exhaustive()
    }
}

impl<P: Paragraph> State<P> {
    /// Returns `true` if the widget currently has focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Returns `true` if focus was gained via keyboard navigation (Tab).
    pub fn is_keyboard_focused(&self) -> bool {
        self.keyboard_focused
    }

    /// Clears focus, selection, and all interaction state.
    pub fn clear_focus(&mut self) {
        self.focused = false;
        self.keyboard_focused = false;
        self.context_menu_position = None;
        if let Some(sel) = &mut self.selection {
            sel.clear();
        }
    }

    /// Returns the context menu position, if a context menu should be shown.
    pub fn context_menu_position(&self) -> Option<Point> {
        self.context_menu_position
    }

    /// Sets or clears the context menu position.
    pub fn set_context_menu_position(&mut self, pos: Option<Point>) {
        self.context_menu_position = pos;
    }

    // The three accessors below exist because `selection` and
    // `clipboard_has_text` are private and the `HasSelectableText` impl lives
    // in the parent module.

    /// The selection as an ordered grapheme range, or `None` if empty.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let sel = self.selection.as_ref()?;
        let left = sel.anchor.min(sel.end);
        let right = sel.anchor.max(sel.end);
        (left != right).then_some((left, right))
    }

    /// Selects the whole content.
    pub fn select_all(&mut self) {
        let count = self.paragraph.content().graphemes(true).count();
        let sel = self
            .selection
            .get_or_insert_with(|| Box::new(SelectionState::default()));
        sel.anchor = 0;
        sel.end = count;
    }

    /// Whether the clipboard held text when the context menu was requested.
    pub fn has_clipboard_text(&self) -> bool {
        self.clipboard_has_text
    }
}

impl<P: Paragraph> std::ops::Deref for State<P> {
    type Target = paragraph::Plain<P>;

    fn deref(&self) -> &paragraph::Plain<P> {
        &self.paragraph
    }
}

impl<P: Paragraph> std::ops::DerefMut for State<P> {
    fn deref_mut(&mut self) -> &mut paragraph::Plain<P> {
        &mut self.paragraph
    }
}

#[derive(Debug, Clone, Default)]
struct SelectionState {
    anchor: usize,
    end: usize,
    dragging: bool,
    modifiers: keyboard::Modifiers,
    last_click: Option<click::Click>,
}

impl SelectionState {
    fn clear(&mut self) {
        self.anchor = 0;
        self.end = 0;
        self.dragging = false;
    }
}

impl<P: Paragraph> iced_core::widget::operation::Focusable for State<P> {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
        self.keyboard_focused = true;
    }

    fn unfocus(&mut self) {
        self.clear_focus();
    }
}

// Concrete in `crate::ui::Renderer`: the selection highlight needs
// `Paragraph::highlight`, which the `text::Paragraph` trait does not have, so
// it is reached on the concrete paragraph type through
// `crate::ui::widget::paragraph`.
type Renderer = crate::ui::Renderer;

impl<Message, Theme> Widget<Message, Theme, Renderer> for Text<'_, Theme, Renderer>
where
    Theme: Catalog,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<crate::ui::widget::paragraph::Paragraph>::default())
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.format.width,
            height: self.format.height,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();

        layout(
            &mut state.paragraph,
            renderer,
            limits,
            &self.fragment,
            self.format,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let style = theme.style(&self.class);
        let bounds = layout.bounds();
        let paragraph = state.paragraph.raw();
        let anchor = bounds.anchor(
            paragraph.min_bounds(),
            paragraph.align_x(),
            paragraph.align_y(),
        );

        let rects: Vec<Rectangle> = state
            .selection
            .as_ref()
            .filter(|sel| sel.anchor != sel.end)
            .map(|sel| {
                let content: &str = self.fragment.as_ref();
                let lo_byte = grapheme_to_byte(content, sel.anchor.min(sel.end));
                let hi_byte = grapheme_to_byte(content, sel.anchor.max(sel.end));

                crate::ui::widget::paragraph::highlight(
                    paragraph,
                    0,
                    (lo_byte, Affinity::After),
                    (hi_byte, Affinity::Before),
                )
                .into_iter()
                .map(|r| Rectangle {
                    x: anchor.x + r.x,
                    y: anchor.y + r.y,
                    width: r.width,
                    height: r.height,
                })
                .collect()
            })
            .unwrap_or_default();

        let fill_selection = |renderer: &mut Renderer| {
            for r in &rects {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: *r,
                        ..renderer::Quad::default()
                    },
                    style.selected_fill,
                );
            }
        };

        if style.selected_text_color.is_none() {
            fill_selection(renderer);
        }
        draw(renderer, defaults, bounds, paragraph, style, viewport);
        if let Some(color) = style.selected_text_color {
            fill_selection(renderer);
            for r in &rects {
                renderer.with_layer(*r, |renderer| {
                    renderer.fill_paragraph(paragraph, anchor, color, *viewport);
                });
            }
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if !self.selectable {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let content: &str = self.fragment.as_ref();
        let grapheme_count = content.graphemes(true).count();
        let paragraph = state.paragraph.raw();

        // Any click outside this widget clears selection and focus.
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(touch::Event::FingerPressed { .. })
        ) && cursor.position_over(bounds).is_none()
        {
            let was_visible = state.focused
                || state.keyboard_focused
                || state
                    .selection
                    .as_ref()
                    .is_some_and(|sel| sel.anchor != sel.end);

            state.focused = false;
            state.keyboard_focused = false;
            if let Some(sel) = &mut state.selection {
                sel.clear();
            }

            if was_visible {
                shell.request_redraw();
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if state.context_menu_position.take().is_some() {
                    shell.capture_event();
                    return;
                }

                if let Some(pos) = cursor.position_over(bounds) {
                    let sel = state
                        .selection
                        .get_or_insert_with(|| Box::new(SelectionState::default()));

                    let anchor = bounds.anchor(
                        paragraph.min_bounds(),
                        paragraph.align_x(),
                        paragraph.align_y(),
                    );
                    let relative = Point::new(pos.x - anchor.x, pos.y - anchor.y);

                    let grapheme_pos = hit_to_grapheme(paragraph, relative, content);

                    let new_click =
                        click::Click::new(pos, mouse::Button::Left, sel.last_click.take());

                    match new_click.kind() {
                        click::Kind::Single => {
                            if sel.modifiers.shift() {
                                sel.end = grapheme_pos;
                            } else {
                                sel.anchor = grapheme_pos;
                                sel.end = grapheme_pos;
                            }
                            sel.dragging = true;
                        }
                        click::Kind::Double => {
                            sel.anchor = previous_start_of_word(content, grapheme_pos);
                            sel.end = next_end_of_word(content, grapheme_pos);
                            sel.dragging = true;
                        }
                        click::Kind::Triple => {
                            sel.anchor = 0;
                            sel.end = grapheme_count;
                            sel.dragging = true;
                        }
                    }

                    sel.last_click = Some(new_click);
                    state.focused = true;
                    state.keyboard_focused = false;
                    shell.capture_event();
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                if let Some(pos) = cursor.position_over(bounds) {
                    state.context_menu_position = Some(pos);
                    state.clipboard_has_text = super::clipboard_has_text(clipboard);
                    state.focused = true;
                    state.keyboard_focused = false;
                    shell.capture_event();
                    shell.request_redraw();
                }
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => {
                if let Some(sel) = &mut state.selection {
                    sel.dragging = false;
                }
            }

            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                if let Some(sel) = &mut state.selection
                    && sel.dragging
                {
                    let anchor = bounds.anchor(
                        paragraph.min_bounds(),
                        paragraph.align_x(),
                        paragraph.align_y(),
                    );
                    let relative = Point::new(position.x - anchor.x, position.y - anchor.y);

                    let end = hit_to_grapheme(paragraph, relative, content);

                    // Only the drag reaching a new grapheme changes what
                    // is drawn; plain motion inside one does not.
                    if end != sel.end {
                        sel.end = end;
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }

            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                physical_key,
                ..
            }) => {
                if !state.focused {
                    return;
                }
                let sel = state
                    .selection
                    .get_or_insert_with(|| Box::new(SelectionState::default()));

                if modifiers.command() {
                    match key.to_latin(*physical_key) {
                        Some('c') => {
                            let left = sel.anchor.min(sel.end);
                            let right = sel.anchor.max(sel.end);
                            if left != right {
                                let selected: String = content
                                    .graphemes(true)
                                    .skip(left)
                                    .take(right - left)
                                    .collect();
                                clipboard.write(iced_core::clipboard::Kind::Standard, selected);
                            }
                            shell.capture_event();
                            return;
                        }
                        Some('a') => {
                            sel.anchor = 0;
                            sel.end = grapheme_count;
                            shell.capture_event();
                            shell.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }

                match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => {
                        let by_word = is_jump_modifier(*modifiers);
                        if modifiers.shift() {
                            sel.end = if by_word {
                                previous_start_of_word(content, sel.end)
                            } else {
                                sel.end.saturating_sub(1)
                            };
                        } else {
                            let left = sel.anchor.min(sel.end);
                            let pos = if by_word {
                                previous_start_of_word(content, left)
                            } else {
                                left.saturating_sub(1)
                            };
                            sel.anchor = pos;
                            sel.end = pos;
                        }
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    keyboard::Key::Named(keyboard::key::Named::ArrowRight) => {
                        let by_word = is_jump_modifier(*modifiers);
                        if modifiers.shift() {
                            sel.end = if by_word {
                                next_end_of_word(content, sel.end)
                            } else {
                                (sel.end + 1).min(grapheme_count)
                            };
                        } else {
                            let right = sel.anchor.max(sel.end);
                            let pos = if by_word {
                                next_end_of_word(content, right)
                            } else {
                                (right + 1).min(grapheme_count)
                            };
                            sel.anchor = pos;
                            sel.end = pos;
                        }
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    keyboard::Key::Named(keyboard::key::Named::Home) => {
                        if modifiers.shift() {
                            sel.end = 0;
                        } else {
                            sel.anchor = 0;
                            sel.end = 0;
                        }
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    keyboard::Key::Named(keyboard::key::Named::End) => {
                        if modifiers.shift() {
                            sel.end = grapheme_count;
                        } else {
                            sel.anchor = grapheme_count;
                            sel.end = grapheme_count;
                        }
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    _ => {}
                }
            }

            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                if let Some(sel) = &mut state.selection {
                    sel.modifiers = *modifiers;
                }
            }

            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.selectable && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation<()>,
    ) {
        operation.text(None, layout.bounds(), &self.fragment);

        if self.selectable {
            let state = tree.state.downcast_mut::<State>();
            operation.focusable(None, layout.bounds(), state);
        }
    }
}

/// The format of some [`Text`].
///
/// Check out the methods of the [`Text`] widget
/// to learn more about each field.
#[derive(Debug, Clone, Copy)]
#[allow(missing_docs)]
pub struct Format<Font> {
    pub width: Length,
    pub height: Length,
    pub size: Option<Pixels>,
    pub font: Option<Font>,
    pub line_height: LineHeight,
    pub align_x: text::Alignment,
    pub align_y: alignment::Vertical,
    pub shaping: Shaping,
    pub wrapping: Wrapping,
}

impl<Font> Default for Format<Font> {
    fn default() -> Self {
        Self {
            size: None,
            line_height: LineHeight::default(),
            font: None,
            width: Length::Shrink,
            height: Length::Shrink,
            align_x: text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::default(),
            wrapping: Wrapping::default(),
        }
    }
}

/// Produces the [`layout::Node`] of a [`Text`] widget.
pub fn layout<Renderer>(
    paragraph: &mut paragraph::Plain<Renderer::Paragraph>,
    renderer: &Renderer,
    limits: &layout::Limits,
    content: &str,
    format: Format<Renderer::Font>,
) -> layout::Node
where
    Renderer: text::Renderer,
{
    layout::sized(limits, format.width, format.height, |limits| {
        let bounds = limits.max();

        let size = format.size.unwrap_or_else(|| renderer.default_size());
        let font = format.font.unwrap_or_else(|| renderer.default_font());

        let _ = paragraph.update(text::Text {
            content,
            bounds,
            size,
            line_height: format.line_height,
            font,
            align_x: format.align_x,
            align_y: format.align_y,
            shaping: format.shaping,
            wrapping: format.wrapping,
        });

        paragraph.min_bounds()
    })
}

/// Draws text using the same logic as the [`Text`] widget.
pub fn draw<Renderer>(
    renderer: &mut Renderer,
    style: &renderer::Style,
    bounds: Rectangle,
    paragraph: &Renderer::Paragraph,
    appearance: Style,
    viewport: &Rectangle,
) where
    Renderer: text::Renderer,
{
    let anchor = bounds.anchor(
        paragraph.min_bounds(),
        paragraph.align_x(),
        paragraph.align_y(),
    );

    renderer.fill_paragraph(
        paragraph,
        anchor,
        appearance.color.unwrap_or(style.text_color),
        *viewport,
    );
}

impl<'a, Message, Theme> From<Text<'a, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
{
    fn from(text: Text<'a, Theme, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(text)
    }
}

impl<'a, Theme, Renderer> From<&'a str> for Text<'a, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Renderer: text::Renderer,
{
    fn from(content: &'a str) -> Self {
        Self::new(content)
    }
}

// There is no `impl From<&str> for Element` here: both types are foreign, so
// the orphan rule forbids it. iced's own impl applies and yields its plain,
// non-selectable `Text`.

/// The appearance of some text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The [`Color`] of the text.
    ///
    /// The default, `None`, means using the inherited color.
    pub color: Option<Color>,
    /// The fill [`Color`] of the selection highlight.
    pub selected_fill: Color,
    /// The [`Color`] of selected text.
    ///
    /// The default, `None`, keeps the regular text color.
    pub selected_text_color: Option<Color>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: None,
            selected_fill: DEFAULT_SELECTION_COLOR,
            selected_text_color: None,
        }
    }
}

/// The theme catalog of a [`Text`].
pub trait Catalog: Sized {
    /// The item class of this [`Catalog`].
    type Class<'a>;

    /// The default class produced by this [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;

    /// The [`Style`] of a class with the given status.
    fn style(&self, item: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`Text`].
///
/// This is just a boxed closure: `Fn(&Theme, Status) -> Style`.
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

const DEFAULT_SELECTION_COLOR: Color = Color {
    r: 0.0,
    g: 0.47,
    b: 0.84,
    a: 0.3,
};

pub(super) fn grapheme_to_byte(content: &str, grapheme_index: usize) -> usize {
    content
        .graphemes(true)
        .take(grapheme_index)
        .map(|g| g.len())
        .sum()
}

fn hit_to_grapheme<P: Paragraph>(paragraph: &P, point: Point, content: &str) -> usize {
    match paragraph.hit_test(point) {
        Some(hit) => {
            let byte_offset = hit.cursor().min(content.len());
            content[..byte_offset].graphemes(true).count()
        }
        None => content.graphemes(true).count(),
    }
}

fn previous_start_of_word(content: &str, grapheme_index: usize) -> usize {
    let graphemes: Vec<&str> = content.graphemes(true).collect();
    let clamped = grapheme_index.min(graphemes.len());
    let before: String = graphemes[..clamped].concat();

    UnicodeSegmentation::split_word_bound_indices(&*before)
        .rfind(|(_, word)| !word.trim_start().is_empty())
        .map_or(0, |(i, prev_word)| {
            clamped
                - prev_word.graphemes(true).count()
                - before[i + prev_word.len()..].graphemes(true).count()
        })
}

fn next_end_of_word(content: &str, grapheme_index: usize) -> usize {
    let graphemes: Vec<&str> = content.graphemes(true).collect();
    let clamped = grapheme_index.min(graphemes.len());
    let after: String = graphemes[clamped..].concat();

    UnicodeSegmentation::split_word_bound_indices(&*after)
        .find(|(_, word)| !word.trim_start().is_empty())
        .map_or(graphemes.len(), |(i, next_word)| {
            clamped + next_word.graphemes(true).count() + after[..i].graphemes(true).count()
        })
}

fn is_jump_modifier(modifiers: keyboard::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.alt()
    } else {
        modifiers.control()
    }
}
