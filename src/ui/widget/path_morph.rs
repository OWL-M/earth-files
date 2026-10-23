// SPDX-License-Identifier: GPL-3.0-only

//! The header's crumbs and its path field, and the glide between them.
//!
//! [`PathMorph`] holds the crumb pill and, while the location is edited, the
//! path field. At rest it shows one of them. When the field opens, the
//! crumbs' names glide to where the same names sit in the field's text: a
//! "›" turns into the "/" before the name after it, the part of the path the
//! crumbs leave out fades in around them, and the pill turns into the
//! field's box. Closing plays the same back, from the field's last text into
//! the crumbs then shown, which may be another folder's: a name glides only
//! where the text holds that crumb's own folder, and everything else fades
//! in or out where it stands. A place with no path behind its crumb (the
//! trash, recents, a network share) has nothing to glide, so it crossfades.
//!
//! One spring drives it all, from 0 (the crumbs) to 1 (the field). The
//! names, the box and the icon are drawn by this widget while it moves; the
//! crumbs and the field themselves are drawn only at rest, while they get
//! their events throughout, so typing right after the click reaches the
//! field. A change of the field's text while it opens ends the glide at
//! once: the text the names glide into would no longer be the field's.
//!
//! The width springs with it, from the crumbs' to the field's. The field is
//! as wide as its text wants, never narrower than the crumbs and never wider
//! than the room it is given. While the header measures its centre (see
//! [`header_bar::measuring`](super::header_bar::measuring)) it is only as
//! wide as the crumbs, so the menus do not fold or unfold as the field
//! opens, closes or is typed into.

use std::ops::Range;
use std::time::{Duration, Instant};

use iced_core::Renderer as _;
use iced_core::event::Event;
use iced_core::text::{self, Paragraph as _, Renderer as _};
use iced_core::widget::{Id, Operation, Tree, tree};
use iced_core::{
    Background, Border, Clipboard, Color, Element, Font, Layout, Length, Pixels, Point, Radians,
    Rectangle, Shell, Size, Vector, Widget, alignment, layout, mouse, overlay, renderer, window,
};
use iced_texture_cache::iced_animate::{Spring, SpringParams};

use crate::ui::convert::ToColor as _;
use crate::ui::widget::icon;
use crate::ui::widget::text_input::StyleSheet as _;
use crate::ui::{Renderer, Theme};

/// The glide's spring: barely any overshoot.
const SPRING: SpringParams = SpringParams::new(0.1, Duration::from_millis(400));

/// How close to its end the progress has to be to stop.
const SETTLED: f32 = 0.002;

/// A frame longer than this is advanced as this, as in `spring_height`.
const MAX_FRAME: f32 = 1.0 / 15.0;

/// The size of the crumbs' names and of the field's text.
pub const TEXT_SIZE: f32 = 14.0;

/// The crumbs' line height (`widget::text::body` and `heading`).
const CRUMB_LINE_HEIGHT: f32 = 21.0;

/// Room after the field's text for its cursor.
const CURSOR: f32 = 2.0;

/// One crumb, as the glide needs it.
#[derive(Clone, Debug)]
pub struct Name {
    /// What the crumb shows.
    pub label: String,
    /// The crumb's folder, as the field writes it; `None` for a place with
    /// no path.
    pub path: Option<String>,
}

/// What the glide needs to know of the crumbs: what they say, where their
/// names sit inside them and the icon before them.
///
/// The crumbs must be a container (the pill) around a
/// [`Crumbs`](super::Crumbs) holding one crumb per name, the last drawn bold
/// as the current folder; the glide finds its pieces in that layout.
#[derive(Clone, Debug, Default)]
pub struct Names {
    /// The crumbs from the outermost folder to the current one.
    pub crumbs: Vec<Name>,
    /// The icon before them, drawn in the accent colour.
    pub icon: Option<icon::Handle>,
    /// How far in from a crumb's left edge its name starts.
    pub crumb_inset: f32,
    /// How far in from the field's left edge its text starts.
    pub field_inset: f32,
}

/// The crumbs, shown until [`PathMorph::field`] gives a field. See the
/// [module documentation](self).
pub fn path_morph<'a, Message: 'a>(
    crumbs: impl Into<Element<'a, Message, Theme, Renderer>>,
    names: Names,
) -> PathMorph<'a, Message> {
    PathMorph {
        crumbs: crumbs.into(),
        field: crate::ui::widget::space::horizontal()
            .width(Length::Fixed(0.0))
            .into(),
        editing: None,
        names,
    }
}

#[allow(missing_debug_implementations)]
pub struct PathMorph<'a, Message> {
    crumbs: Element<'a, Message, Theme, Renderer>,
    /// The field, or an empty stand-in while there is none.
    field: Element<'a, Message, Theme, Renderer>,
    editing: Option<Editing>,
    names: Names,
}

/// The field being shown.
struct Editing {
    /// Its text.
    text: String,
    /// Its text input's id, by which its box is found.
    id: Id,
}

impl<'a, Message> PathMorph<'a, Message> {
    /// Shows `field` in place of the crumbs: its text is `text`, and its
    /// text input has `id`.
    #[must_use]
    pub fn field(
        mut self,
        field: impl Into<Element<'a, Message, Theme, Renderer>>,
        text: String,
        id: Id,
    ) -> Self {
        self.field = field.into();
        self.editing = Some(Editing { text, id });
        self
    }

    /// The children shown at rest and given events: the field while there
    /// is one, the crumbs otherwise.
    const fn live(&self) -> usize {
        if self.editing.is_some() { 1 } else { 0 }
    }
}

#[derive(Default)]
struct State {
    /// The progress from the crumbs (0) to the field (1).
    spring: Option<Spring>,
    /// When the last frame advanced the spring; `None` while it is at rest.
    last_frame: Option<Instant>,
    /// The field's last text, kept to glide back from once it is gone.
    text: String,
    /// The field's last width.
    field_width: f32,
    /// The field's text input, within the field, at its last layout.
    field_box: Rectangle,
}

impl State {
    fn progress(&self) -> f32 {
        self.spring.map_or(0.0, |spring| spring.position())
    }

    fn opening(&self) -> bool {
        self.spring.is_some_and(|spring| spring.target() > 0.5)
    }

    fn is_moving(&self) -> bool {
        self.spring
            .is_some_and(|spring| !spring.is_settled_within(SETTLED))
    }
}

/// Finds the bounds of the container with `id`.
struct FindBox<'a> {
    id: &'a Id,
    found: Option<Rectangle>,
}

impl Operation for FindBox<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if self.found.is_none() && id == Some(self.id) {
            self.found = Some(bounds);
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for PathMorph<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.crumbs), Tree::new(&self.field)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.crumbs, &self.field]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let crumbs = self
            .crumbs
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let Size {
            width: kept,
            height,
        } = crumbs.size();
        let measuring = super::header_bar::measuring();

        let state = tree.state.downcast_mut::<State>();
        let target = if self.editing.is_some() { 1.0 } else { 0.0 };
        let spring = state
            .spring
            .get_or_insert_with(|| Spring::new(SPRING, target));
        if spring.target() != target {
            spring.set_target(target);
        } else if let Some(editing) = &self.editing
            && editing.text != state.text
        {
            // Typed into while it opens: the names would glide into text
            // the field no longer holds
            spring.snap();
        }
        if let Some(editing) = &self.editing {
            state.text.clone_from(&editing.text);
            let wanted = text_width(&editing.text) + 2.0 * self.names.field_inset + CURSOR;
            state.field_width = wanted.min(limits.max().width).max(kept);
        }
        let field_width = state.field_width.max(kept);
        let width = if measuring {
            kept
        } else {
            (kept + (field_width - kept) * state.progress()).clamp(0.0, limits.max().width)
        };

        let field = self.field.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(field_width, height)),
        );
        if let Some(editing) = &self.editing {
            let mut find = FindBox {
                id: &editing.id,
                found: None,
            };
            self.field.as_widget_mut().operate(
                &mut tree.children[1],
                Layout::new(&field),
                renderer,
                &mut find,
            );
            let state = tree.state.downcast_mut::<State>();
            state.field_box = find
                .found
                .unwrap_or_else(|| Rectangle::new(Point::ORIGIN, field.size()));
        }
        layout::Node::with_children(Size::new(width, height), vec![crumbs, field])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let live = self.live();
        let child = if live == 1 {
            &mut self.field
        } else {
            &mut self.crumbs
        };
        operation.traverse(&mut |operation| {
            child.as_widget_mut().operate(
                &mut tree.children[live],
                layout.children().nth(live).unwrap(),
                renderer,
                operation,
            );
        });
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
        let state = tree.state.downcast_mut::<State>();
        if state.is_moving() {
            if let Event::Window(window::Event::RedrawRequested(now)) = event {
                let dt = state.last_frame.map_or(0.0, |last_frame| {
                    now.saturating_duration_since(last_frame)
                        .as_secs_f32()
                        .min(MAX_FRAME)
                });
                if let Some(spring) = &mut state.spring {
                    spring.tick(dt);
                    if spring.is_settled_within(SETTLED) {
                        spring.snap();
                    }
                }
                state.last_frame = state.is_moving().then_some(*now);
                shell.invalidate_layout();
            }
            // Asked for on any event, so a glide that has just begun gets
            // its first frame
            if state.is_moving() {
                shell.request_redraw();
            }
        }

        let live = self.live();
        let child = if live == 1 {
            &mut self.field
        } else {
            &mut self.crumbs
        };
        child.as_widget_mut().update(
            &mut tree.children[live],
            event,
            layout.children().nth(live).unwrap(),
            cursor,
            renderer,
            clipboard,
            shell,
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
        let live = self.live();
        let child = if live == 1 { &self.field } else { &self.crumbs };
        child.as_widget().mouse_interaction(
            &tree.children[live],
            layout.children().nth(live).unwrap(),
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
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        if state.is_moving() {
            self.draw_glide(state, renderer, theme, style, layout, viewport);
            return;
        }
        let live = self.live();
        let child = if live == 1 { &self.field } else { &self.crumbs };
        child.as_widget().draw(
            &tree.children[live],
            renderer,
            theme,
            style,
            layout.children().nth(live).unwrap(),
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
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let live = self.live();
        let child = if live == 1 {
            &mut self.field
        } else {
            &mut self.crumbs
        };
        child.as_widget_mut().overlay(
            &mut tree.children[live],
            layout.children().nth(live).unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<Message> PathMorph<'_, Message> {
    /// Draws the glide at the spring's progress.
    fn draw_glide(
        &self,
        state: &State,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        viewport: &Rectangle,
    ) {
        let p = state.progress();
        let t = p.clamp(0.0, 1.0);
        let bounds = layout.bounds();
        let pill = layout.children().next().unwrap();

        // The pill turning into the field's box, both as wide as the widget
        // is now
        let fb = state.field_box;
        let pill_box = Rectangle {
            x: bounds.x,
            y: pill.bounds().y,
            width: bounds.width,
            height: pill.bounds().height,
        };
        let field_box = Rectangle {
            x: bounds.x + fb.x,
            y: bounds.y + fb.y,
            width: (bounds.width - (state.field_width - fb.width)).max(0.0),
            height: fb.height,
        };
        let pill_style = <Theme as crate::ui::iced::widget::container::Catalog>::style(
            theme,
            &crate::ui::theme::Container::Pill,
        );
        if let Some(background) = pill_style.background {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: lerp_rect(pill_box, field_box, t),
                    border: Border {
                        radius: pill_style.border.radius,
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                faded_background(background, 1.0 - t),
            );
        }
        let field_style = if state.opening() {
            theme.focused(&crate::ui::theme::TextInput::Search)
        } else {
            theme.active(&crate::ui::theme::TextInput::Search)
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: lerp_rect(pill_box, field_box, t),
                border: Border {
                    radius: field_style.border_radius,
                    width: field_style.border_width,
                    color: faded(field_style.border_color, t),
                },
                ..renderer::Quad::default()
            },
            faded_background(field_style.background, t),
        );

        // The crumbs' pieces: the icon, then the crumbs, separators and
        // "more"s, as `Crumbs` lays them out
        let names = &self.names.crumbs;
        let n = names.len();
        let parts: Vec<Rectangle> = pill
            .children()
            .next()
            .map(|crumbs| crumbs.children().map(|part| part.bounds()).collect())
            .unwrap_or_default();
        if n == 0 || parts.len() != 3 * n - 1 {
            return;
        }

        if let Some(handle) = &self.names.icon {
            let gone = smooth(t / 0.5);
            let scale = 1.0 - 0.4 * gone;
            let at = parts[0];
            let size = Size::new(at.width * scale, at.height * scale);
            let icon_bounds = Rectangle::new(
                Point::new(
                    at.center_x() - size.width / 2.0,
                    at.center_y() - size.height / 2.0,
                ),
                size,
            );
            draw_icon(
                renderer,
                handle,
                icon_bounds,
                theme.cosmic().accent_text_color().to_color(),
                1.0 - gone,
            );
        }

        let crumb_color = style.text_color;
        let field_color = field_style.text_color.unwrap_or(style.text_color);
        let mut separator_color = crumb_color;
        separator_color.a *= 0.45;

        // Where a piece of the crumbs is, what it says, in which font and
        // colour
        let source = |source: Source| -> (f32, f32, &str, Font, Color) {
            let inset = self.names.crumb_inset;
            match source {
                Source::Crumb(i) => {
                    let at = parts[1 + i];
                    let font = if i + 1 == n {
                        crate::ui::font::bold()
                    } else {
                        crate::ui::font::default()
                    };
                    (
                        at.x + inset,
                        at.center_y(),
                        names[i].label.as_str(),
                        font,
                        crumb_color,
                    )
                }
                Source::Separator(j) => {
                    let at = parts[1 + n + j];
                    (
                        at.x,
                        at.center_y(),
                        "›",
                        crate::ui::font::default(),
                        separator_color,
                    )
                }
                Source::More(j) => {
                    let at = parts[2 * n + j];
                    (
                        at.x + inset,
                        at.center_y(),
                        "...",
                        crate::ui::font::default(),
                        crumb_color,
                    )
                }
            }
        };

        // Where a piece of the field's text is: its text as laid out once
        // the field is open, scrolled to its end when it does not fit
        let text = state.text.as_str();
        let room = fb.width - 2.0 * self.names.field_inset;
        let scroll = (text_width(text) + CURSOR - room).max(0.0);
        let text_x = bounds.x + fb.x + self.names.field_inset - scroll;
        let text_y = bounds.y + fb.y + fb.height / 2.0;
        let target = |range: &Range<usize>| text_x + text_width(&text[..range.start]);

        let shown = |source: Source| {
            let at = match source {
                Source::Crumb(i) => parts[1 + i],
                Source::Separator(j) => parts[1 + n + j],
                Source::More(j) => parts[2 * n + j],
            };
            at.width > 0.0
        };
        // Only inside the box as it morphs: from the whole pill, where the
        // crumbs are drawn, to the field's text area within its padding,
        // where the field clips its own text. A path scrolled to its end
        // would otherwise paint over what is beside the field.
        let inset = self.names.field_inset;
        let text_area = Rectangle {
            x: field_box.x + inset,
            width: (field_box.width - 2.0 * inset).max(0.0),
            ..field_box
        };
        let Some(clip) = lerp_rect(pill_box, text_area, t).intersection(viewport) else {
            return;
        };
        // A layer, which clips what is drawn in it: the bounds `fill_text`
        // takes only leave out text wholly outside them
        renderer.with_layer(clip, |renderer| {
            for piece in plan(names, &shown, text) {
                match piece {
                    Piece::Glide { from, to } => {
                        let (x0, y0, label, font, color) = source(from);
                        let x1 = target(&to);
                        let x = x0 + (x1 - x0) * p;
                        let y = y0 + (text_y - y0) * t;
                        let goes_to = &text[to];
                        let (out, into) = if label == goes_to {
                            (1.0 - t, t)
                        } else {
                            (1.0 - smooth(t / 0.5), smooth((t - 0.4) / 0.6))
                        };
                        draw_text(
                            renderer,
                            label,
                            font,
                            crumb_line_height(),
                            x,
                            y,
                            faded(color, out),
                        );
                        draw_text(
                            renderer,
                            goes_to,
                            crate::ui::font::default(),
                            text::LineHeight::Relative(1.0),
                            x,
                            y,
                            faded(field_color, into),
                        );
                    }
                    Piece::Leave(from) => {
                        let (x, y, label, font, color) = source(from);
                        let out = 1.0 - smooth(t / 0.6);
                        draw_text(
                            renderer,
                            label,
                            font,
                            crumb_line_height(),
                            x,
                            y,
                            faded(color, out),
                        );
                    }
                    Piece::Arrive(to) => {
                        let into = smooth((t - 0.3) / 0.7);
                        draw_text(
                            renderer,
                            &text[to.clone()],
                            crate::ui::font::default(),
                            text::LineHeight::Relative(1.0),
                            target(&to),
                            text_y,
                            faded(field_color, into),
                        );
                    }
                }
            }
        });
    }
}

/// A piece of the crumbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The name of the crumb at this index.
    Crumb(usize),
    /// The "›" before the crumb after this index.
    Separator(usize),
    /// The "..." standing for the crumbs up to this index.
    More(usize),
}

/// What one piece of text does in the glide.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    /// Moves from a piece of the crumbs to this part of the field's text.
    Glide { from: Source, to: Range<usize> },
    /// Fades out where it is in the crumbs.
    Leave(Source),
    /// Fades in where it is in the field's text.
    Arrive(Range<usize>),
}

/// Where the name of the folder at `path` is in `text`, when `text` runs
/// through that folder: `text` is `path` or goes on below it.
fn component(path: &str, text: &str) -> Option<Range<usize>> {
    let path = path.trim_end_matches('/');
    let rest = text.strip_prefix(path)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let start = path.rfind('/').map_or(0, |i| i + 1);
    (start < path.len()).then_some(start..path.len())
}

/// The glide between the crumbs `names` and the field's `text`, where
/// `shown` tells which pieces of the crumbs are on show.
///
/// A shown crumb whose folder `text` runs through glides to that folder's
/// name in it, and the "›" before it to the "/" before that name. Every
/// other shown piece of the crumbs leaves, and every part of `text` nothing
/// glides to arrives.
fn plan(names: &[Name], shown: &dyn Fn(Source) -> bool, text: &str) -> Vec<Piece> {
    let n = names.len();
    let targets: Vec<Option<Range<usize>>> = names
        .iter()
        .map(|name| name.path.as_deref().and_then(|path| component(path, text)))
        .collect();

    let mut pieces = Vec::new();
    let mut covered = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        let from = Source::Crumb(i);
        if !shown(from) {
            continue;
        }
        match target {
            Some(to) => {
                covered.push(to.clone());
                pieces.push(Piece::Glide {
                    from,
                    to: to.clone(),
                });
            }
            None => pieces.push(Piece::Leave(from)),
        }
    }
    for j in 0..n.saturating_sub(1) {
        let from = Source::Separator(j);
        if !shown(from) {
            continue;
        }
        let slash = targets[j + 1]
            .as_ref()
            .filter(|_| shown(Source::Crumb(j + 1)))
            .filter(|to| to.start > 0 && text[..to.start].ends_with('/'))
            .map(|to| to.start - 1..to.start);
        match slash {
            Some(to) => {
                covered.push(to.clone());
                pieces.push(Piece::Glide { from, to });
            }
            None => pieces.push(Piece::Leave(from)),
        }
        if shown(Source::More(j)) {
            pieces.push(Piece::Leave(Source::More(j)));
        }
    }

    covered.sort_by_key(|range| range.start);
    let mut at = 0;
    for range in covered {
        if range.start > at {
            pieces.push(Piece::Arrive(at..range.start));
        }
        at = at.max(range.end);
    }
    if at < text.len() {
        pieces.push(Piece::Arrive(at..text.len()));
    }
    pieces
}

/// `content` laid out on one line at the crumbs' and field's text size.
fn paragraph(
    content: &str,
    font: Font,
    line_height: text::LineHeight,
) -> <Renderer as text::Renderer>::Paragraph {
    <Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content,
        bounds: Size::INFINITE,
        size: Pixels(TEXT_SIZE),
        line_height,
        font,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    })
}

/// How wide the field lays `content` out.
fn text_width(content: &str) -> f32 {
    paragraph(
        content,
        crate::ui::font::default(),
        text::LineHeight::Relative(1.0),
    )
    .min_width()
}

const fn crumb_line_height() -> text::LineHeight {
    text::LineHeight::Absolute(Pixels(CRUMB_LINE_HEIGHT))
}

/// Draws `content` starting at `left`, its line centred on `center_y`.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    renderer: &mut Renderer,
    content: &str,
    font: Font,
    line_height: text::LineHeight,
    left: f32,
    center_y: f32,
    color: Color,
) {
    if color.a <= 0.0 || content.is_empty() {
        return;
    }
    // Measured as a paragraph, but drawn as text the renderer owns: it
    // holds a drawn paragraph only weakly, and this one would be gone
    // before the frame is rendered
    let bounds = paragraph(content, font, line_height).min_bounds();
    let position = Point::new(left, center_y - bounds.height / 2.0);
    renderer.fill_text(
        text::Text {
            content: content.to_owned(),
            bounds,
            size: Pixels(TEXT_SIZE),
            line_height,
            font,
            align_x: text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        },
        position,
        color,
        // Where the text is, not where it may show: the renderer masks it
        // to the layer it is drawn in only where it runs outside that
        // layer, and takes these bounds to be its extent
        Rectangle::new(position, bounds),
    );
}

/// Draws the icon `handle` in `bounds`, in `color` if it is symbolic.
fn draw_icon(
    renderer: &mut Renderer,
    handle: &icon::Handle,
    bounds: Rectangle,
    color: Color,
    opacity: f32,
) {
    if opacity <= 0.0 {
        return;
    }
    match &handle.data {
        icon::Data::Svg(svg) => iced_core::svg::Renderer::draw_svg(
            renderer,
            iced_core::svg::Svg {
                handle: svg.clone(),
                color: handle.symbolic.then_some(color),
                rotation: Radians(0.0),
                opacity,
            },
            bounds,
            bounds,
        ),
        icon::Data::Image(image) => iced_core::image::Renderer::draw_image(
            renderer,
            iced_core::Image {
                handle: image.clone(),
                filter_method: iced_core::image::FilterMethod::Linear,
                rotation: Radians(0.0),
                border_radius: iced_core::border::Radius::default(),
                opacity,
                snap: true,
            },
            bounds,
            bounds,
        ),
    }
}

/// 0 below 0, 1 above 1, and an S-curve between.
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn faded(mut color: Color, alpha: f32) -> Color {
    color.a *= alpha.clamp(0.0, 1.0);
    color
}

fn faded_background(background: Background, alpha: f32) -> Background {
    match background {
        Background::Color(color) => Background::Color(faded(color, alpha)),
        other => other,
    }
}

fn lerp_rect(a: Rectangle, b: Rectangle, t: f32) -> Rectangle {
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Rectangle {
        x: lerp(a.x, b.x),
        y: lerp(a.y, b.y),
        width: lerp(a.width, b.width),
        height: lerp(a.height, b.height),
    }
}

impl<'a, Message: 'a> From<PathMorph<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(morph: PathMorph<'a, Message>) -> Self {
        Element::new(morph)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use iced_core::{clipboard, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::ui::widget::{container, id_container, mouse_area, space};

    const WINDOW: Size = Size::new(800.0, 40.0);
    const INSET: f32 = 14.0;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Msg {
        Field,
        Crumb,
    }

    fn name(label: &str, path: Option<&str>) -> Name {
        Name {
            label: label.to_owned(),
            path: path.map(str::to_owned),
        }
    }

    /// Two crumbs, "Photos" and "2024", as the header lays them out: in a
    /// pill, around a `Crumbs`.
    fn crumbs() -> Element<'static, Msg, Theme, Renderer> {
        let label = |label: &'static str| -> Element<'static, Msg, Theme, Renderer> {
            mouse_area(container(crate::ui::widget::text::body(label)).padding(4))
                .on_press(Msg::Crumb)
                .into()
        };
        container(
            crate::ui::widget::crumbs(
                space::horizontal()
                    .width(Length::Fixed(16.0))
                    .height(Length::Fixed(16.0)),
                vec![label("Photos"), label("2024")],
                vec![crate::ui::widget::text::body("›").into()],
                vec![label("...")],
            )
            .spacing(4.0),
        )
        .padding([0, 12])
        .center_y(Length::Fixed(32.0))
        .into()
    }

    fn names() -> Names {
        Names {
            crumbs: vec![
                name("Photos", Some("/m/Photos")),
                name("2024", Some("/m/Photos/2024")),
            ],
            icon: None,
            crumb_inset: 4.0,
            field_inset: INSET,
        }
    }

    /// The width the field wants for `text`.
    fn wanted(text: &str) -> f32 {
        text_width(text) + 2.0 * INSET + CURSOR
    }

    struct Harness {
        /// The field's text, while it is open.
        editing: Option<String>,
        /// Room left empty before it.
        lead: f32,
        published: Vec<Msg>,
        redraw: bool,
        renderer: Renderer,
        cache: Option<Cache>,
        now: Instant,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                editing: None,
                lead: 0.0,
                published: Vec::new(),
                redraw: false,
                renderer: iced_texture_cache::testing::headless_tiny_skia(),
                cache: Some(Cache::default()),
                now: Instant::now(),
            }
        }

        fn view(&self) -> Element<'static, Msg, Theme, Renderer> {
            let mut morph = path_morph(crumbs(), names());
            if let Some(text) = &self.editing {
                morph = morph.field(
                    id_container(
                        mouse_area(
                            space::horizontal()
                                .width(Length::Fill)
                                .height(Length::Fixed(28.0)),
                        )
                        .on_press(Msg::Field),
                        Id::new("field"),
                    ),
                    text.clone(),
                    Id::new("field"),
                );
            }
            crate::ui::widget::Row::new()
                .push(space::horizontal().width(Length::Fixed(self.lead)))
                .push(id_container(morph, Id::new("box")))
                .into()
        }

        fn with_ui<R>(
            &mut self,
            f: impl FnOnce(&mut UserInterface<'_, Msg, Theme, Renderer>, &mut Renderer) -> R,
        ) -> R {
            let cache = self.cache.take().unwrap();
            let view = self.view();
            let mut ui = UserInterface::build(view, WINDOW, cache, &mut self.renderer);
            let result = f(&mut ui, &mut self.renderer);
            self.cache = Some(ui.into_cache());
            result
        }

        fn send(&mut self, event: Event, cursor: mouse::Cursor) {
            let mut messages = Vec::new();
            let state = self.with_ui(|ui, renderer| {
                let (state, _) = ui.update(
                    &[event],
                    cursor,
                    renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                // Every frame is drawn too, the glide's included
                ui.draw(
                    renderer,
                    &Theme::default(),
                    &renderer::Style::default(),
                    cursor,
                );
                state
            });
            self.redraw = matches!(
                state,
                iced_runtime::user_interface::State::Updated {
                    redraw_request: window::RedrawRequest::NextFrame,
                    ..
                }
            );
            self.published.extend(messages);
        }

        fn frame(&mut self) {
            self.now += Duration::from_millis(16);
            self.send(
                Event::Window(window::Event::RedrawRequested(self.now)),
                mouse::Cursor::Unavailable,
            );
        }

        fn width(&mut self) -> f32 {
            struct Bounds(Option<Rectangle>);
            impl Operation for Bounds {
                fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                    operate(self);
                }
                fn container(&mut self, found: Option<&Id>, bounds: Rectangle) {
                    if found == Some(&Id::new("box")) {
                        self.0 = Some(bounds);
                    }
                }
            }
            self.with_ui(|ui, renderer| {
                let mut bounds = Bounds(None);
                ui.operate(renderer, &mut bounds);
                bounds.0.expect("laid out").width
            })
        }

        /// Runs frames until it stops asking for them, returning its width
        /// after each.
        fn settle(&mut self) -> Vec<f32> {
            let mut seen = Vec::new();
            for _ in 0..200 {
                self.frame();
                seen.push(self.width());
                if !self.redraw {
                    return seen;
                }
            }
            panic!("never settled: {seen:?}");
        }
    }

    const TEXT: &str = "/m/Photos/2024/a folder with a long name";

    #[test]
    fn the_width_springs_from_the_crumbs_to_the_field_and_back() {
        let mut harness = Harness::new();
        harness.frame();
        let crumbs = harness.width();
        assert!(!harness.redraw, "nothing moves at rest");

        harness.editing = Some(TEXT.to_owned());
        harness.frame();
        let seen = harness.settle();
        assert!(
            seen.iter()
                .any(|&w| w > crumbs + 1.0 && w < wanted(TEXT) - 1.0),
            "glided: {seen:?}"
        );
        assert_eq!(*seen.last().unwrap(), wanted(TEXT));

        harness.editing = None;
        harness.frame();
        let seen = harness.settle();
        assert!(
            seen.iter()
                .any(|&w| w > crumbs + 1.0 && w < wanted(TEXT) - 1.0),
            "glided back: {seen:?}"
        );
        assert_eq!(*seen.last().unwrap(), crumbs);
    }

    #[test]
    fn the_glide_paints_nothing_beside_its_box() {
        use iced_core::renderer::Headless as _;

        // A path too long for the room, which the field scrolls to its end
        let mut harness = Harness::new();
        harness.lead = 100.0;
        harness.frame();
        harness.editing = Some(format!("/m/Photos/2024/{}", "a long name ".repeat(20)));
        let background = Color::WHITE;
        let mut glided = 0;
        for _ in 0..40 {
            harness.frame();
            if !harness.redraw {
                break;
            }
            glided += 1;
            let pixels = harness.renderer.screenshot(
                Size::new(WINDOW.width as u32, WINDOW.height as u32),
                1.0,
                background,
            );
            let stray = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(i, px)| (i % WINDOW.width as usize) < 100 && **px != [255, 255, 255, 255])
                .count();
            assert_eq!(stray, 0, "painted before the box on frame {glided}");
        }
        assert!(glided > 3, "it glided");
    }

    #[test]
    fn typing_while_it_opens_ends_the_glide() {
        let mut harness = Harness::new();
        harness.frame();
        harness.editing = Some(TEXT.to_owned());
        harness.frame();
        harness.frame();
        assert!(harness.width() < wanted(TEXT) - 1.0, "still opening");

        let typed = format!("{TEXT}x");
        harness.editing = Some(typed.clone());
        harness.frame();
        assert_eq!(harness.width(), wanted(&typed));
        assert!(!harness.redraw, "at rest");
    }

    #[test]
    fn the_field_takes_presses_while_it_opens() {
        let mut harness = Harness::new();
        harness.frame();
        harness.editing = Some(TEXT.to_owned());
        harness.frame();
        harness.frame();
        // Over the first crumb, which the field now stands over
        harness.send(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            mouse::Cursor::Available(Point::new(40.0, 20.0)),
        );
        assert_eq!(harness.published, [Msg::Field]);
    }

    #[test]
    fn a_header_measuring_it_gets_the_crumbs_width() {
        let mut harness = Harness::new();
        harness.frame();
        let crumbs = harness.width();
        harness.editing = Some(TEXT.to_owned());
        harness.settle();

        let mut morph: Element<'_, Msg, Theme, Renderer> = harness.view();
        let mut tree = Tree::new(&morph);
        let measured = {
            let _measuring = super::super::header_bar::tests_measuring();
            morph
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &harness.renderer,
                    &layout::Limits::new(Size::ZERO, WINDOW),
                )
                .size()
                .width
        };
        assert_eq!(measured, crumbs);
    }

    /// Which pieces show: everything, or with `hidden` leading crumbs
    /// hidden behind the "..." of the last of them.
    fn shown(n: usize, hidden: usize) -> impl Fn(Source) -> bool {
        move |source| match source {
            Source::Crumb(i) => i >= hidden,
            Source::Separator(j) => j + 1 >= hidden.max(1),
            Source::More(j) => hidden > 0 && j + 1 == hidden && j < n,
        }
    }

    fn drive() -> Vec<Name> {
        vec![
            name("Backup", Some("/run/media/user/Backup")),
            name("Photos", Some("/run/media/user/Backup/Photos")),
            name("2024", Some("/run/media/user/Backup/Photos/2024")),
        ]
    }

    #[test]
    fn every_name_glides_into_its_own_folders_path() {
        let text = "/run/media/user/Backup/Photos/2024";
        let at = |s: &str| {
            let start = text.find(s).unwrap();
            start..start + s.len()
        };
        let slash = |s: &str| {
            let start = text.find(s).unwrap() - 1;
            start..start + 1
        };
        assert_eq!(
            plan(&drive(), &shown(3, 0), text),
            [
                Piece::Glide {
                    from: Source::Crumb(0),
                    to: at("Backup")
                },
                Piece::Glide {
                    from: Source::Crumb(1),
                    to: at("Photos")
                },
                Piece::Glide {
                    from: Source::Crumb(2),
                    to: at("2024")
                },
                Piece::Glide {
                    from: Source::Separator(0),
                    to: slash("Photos")
                },
                Piece::Glide {
                    from: Source::Separator(1),
                    to: slash("2024")
                },
                // What the crumbs leave out
                Piece::Arrive(0..at("Backup").start),
            ]
        );
    }

    #[test]
    fn names_the_text_no_longer_runs_through_fade() {
        // Typed into another folder of the drive, then closed
        let text = "/run/media/user/Backup/Music";
        assert_eq!(
            plan(&drive(), &shown(3, 0), text),
            [
                Piece::Glide {
                    from: Source::Crumb(0),
                    to: 16..22
                },
                Piece::Leave(Source::Crumb(1)),
                Piece::Leave(Source::Crumb(2)),
                Piece::Leave(Source::Separator(0)),
                Piece::Leave(Source::Separator(1)),
                Piece::Arrive(0..16),
                Piece::Arrive(22..text.len()),
            ]
        );
    }

    #[test]
    fn a_name_only_matches_a_whole_folder() {
        assert_eq!(component("/a/Photos", "/a/Photos2"), None);
        assert_eq!(component("/a/Photos", "/a/Photos/"), Some(3..9));
        assert_eq!(component("/a/Photos/", "/a/Photos"), Some(3..9));
        // The root has no name to glide to
        assert_eq!(component("/", "/a"), None);
    }

    #[test]
    fn hidden_crumbs_arrive_and_their_dots_leave() {
        let text = "/run/media/user/Backup/Photos/2024";
        let pieces = plan(&drive(), &shown(3, 1), text);
        assert!(
            pieces.contains(&Piece::Leave(Source::More(0))),
            "{pieces:?}"
        );
        assert!(
            pieces.contains(&Piece::Glide {
                from: Source::Separator(0),
                to: 22..23
            }),
            "{pieces:?}"
        );
        // "Backup" is not on show: it arrives with the rest of the prefix
        assert!(pieces.contains(&Piece::Arrive(0..22)), "{pieces:?}");
    }

    #[test]
    fn a_place_with_no_path_crossfades() {
        let names = [name("Trash", None)];
        let text = "trash:///";
        assert_eq!(
            plan(&names, &shown(1, 0), text),
            [Piece::Leave(Source::Crumb(0)), Piece::Arrive(0..text.len())]
        );
    }
}
