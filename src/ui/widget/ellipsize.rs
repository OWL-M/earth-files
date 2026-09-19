// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Ellipsized text, implemented above iced.
//!
//! `iced 0.14.0` has no `text::Ellipsize`, so this widget measures the string
//! with the font, size and shaping it will be drawn with, truncates it on
//! grapheme boundaries, inserts `…`, and hands a plain `String` to iced's own
//! text layout.
//!
//! It reproduces `cosmic-text 0.19`'s algorithm (`shape.rs::layout_middle`
//! and the `max_line_count` path):
//!
//! * `End`: the longest prefix whose width plus the ellipsis fits.
//! * `Middle`: the longest prefix fitting in half the width, then the longest
//!   suffix fitting in whatever is left after the ellipsis.
//! * `Lines(n)`: lines `1..n-1` wrap normally; line `n` is the ellipsized
//!   remainder of the string.

use std::borrow::Cow;

use iced_core::text::{self, Hit, LineHeight, Paragraph, Wrapping, paragraph::Plain};
use iced_core::widget::text::{Catalog, Format, draw as draw_text, layout as layout_text};
use iced_core::widget::{Tree, tree};
use iced_core::{
    Element, Layout, Length, Pixels, Point, Rectangle, Size, Widget, layout, mouse, renderer,
};
use unicode_segmentation::UnicodeSegmentation;

const ELLIPSIS: &str = "…";
/// Slack for float comparisons of text extents, in pixels.
const EPSILON: f32 = 0.5;
/// A probe x far to the right of any line, for the RTL line-start hit test.
const FAR_RIGHT: f32 = 1.0e6;

/// Where the ellipsis goes, and how many lines the text may occupy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keep a head and a tail, `…` between.
    Middle(usize),
    /// Keep a head, `…` at the end.
    End(usize),
}

impl Mode {
    fn lines(self) -> usize {
        match self {
            Self::Middle(lines) | Self::End(lines) => lines.max(1),
        }
    }
}

/// Text that ellipsizes itself to the width it is laid out in.
pub struct Ellipsize<'a, Theme = crate::ui::Theme, Renderer = crate::ui::Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    content: Cow<'a, str>,
    format: Format<Renderer::Font>,
    class: Theme::Class<'a>,
    mode: Mode,
}

impl<'a, Theme, Renderer> Ellipsize<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// Ellipsized text with the renderer's default font and size.
    pub fn new(content: impl Into<Cow<'a, str>>, mode: Mode) -> Self {
        Self {
            content: content.into(),
            format: Format::default(),
            class: Theme::default(),
            mode,
        }
    }

    /// Sets the text size.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.format.size = Some(size.into());
        self
    }

    /// Sets the [`LineHeight`].
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.format.line_height = line_height.into();
        self
    }

    /// Sets the font.
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.format.font = Some(font.into());
        self
    }

    /// Sets the width.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.format.width = width.into();
        self
    }

    /// Sets the height.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.format.height = height.into();
        self
    }

    /// Sets the horizontal alignment.
    pub fn align_x(mut self, align: impl Into<text::Alignment>) -> Self {
        self.format.align_x = align.into();
        self
    }

    /// Sets the [`Wrapping`] strategy.
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.format.wrapping = wrapping;
        self
    }

    /// Sets the style class.
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

/// Per-widget state: the laid out paragraph, a scratch paragraph used only for
/// measuring, and the last resolved string so repeated layouts are free.
struct State<P: Paragraph> {
    paragraph: Plain<P>,
    scratch: Plain<P>,
    cache: Option<(f32, String, String)>,
}

impl<P: Paragraph> Default for State<P> {
    fn default() -> Self {
        Self {
            paragraph: Plain::default(),
            scratch: Plain::default(),
            cache: None,
        }
    }
}

/// Byte offsets of every grapheme boundary in `s`, including `s.len()`.
fn boundaries(s: &str) -> Vec<usize> {
    s.grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect()
}

/// Longest prefix of `s`, on a grapheme boundary, no wider than `budget`.
fn max_prefix(measure: &mut dyn FnMut(&str) -> f32, s: &str, budget: f32) -> usize {
    let bounds = boundaries(s);
    let (mut lo, mut hi) = (0, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&s[..bounds[mid]]) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    bounds[lo]
}

/// Longest suffix of `s`, on a grapheme boundary, no wider than `budget`;
/// returns the byte offset the suffix starts at.
fn max_suffix(measure: &mut dyn FnMut(&str) -> f32, s: &str, budget: f32) -> usize {
    let bounds = boundaries(s);
    let (mut lo, mut hi) = (0, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if measure(&s[bounds[mid]..]) <= budget {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    bounds[lo]
}

/// Ellipsize `s` onto a single line of `width`, as `cosmic-text` would.
fn single_line(measure: &mut dyn FnMut(&str) -> f32, s: &str, mode: Mode, width: f32) -> String {
    let ellipsis = measure(ELLIPSIS);
    match mode {
        Mode::End(_) => {
            let head = max_prefix(measure, s, width - ellipsis);
            format!("{}{ELLIPSIS}", &s[..head])
        }
        Mode::Middle(_) => {
            // cosmic-text lays the head out into half the width with no room
            // reserved for the ellipsis, then fills what is left backwards.
            let head = max_prefix(measure, s, width / 2.0);
            let head_width = measure(&s[..head]);
            let tail = max_suffix(measure, &s[head..], width - head_width - ellipsis) + head;
            format!("{}{ELLIPSIS}{}", &s[..head], &s[tail..])
        }
    }
}

/// How many lines `s` occupies when laid out at `limits`.
fn line_count<Renderer: text::Renderer>(
    renderer: &Renderer,
    scratch: &mut Plain<Renderer::Paragraph>,
    s: &str,
    format: Format<Renderer::Font>,
    limits: &layout::Limits,
    line_height: f32,
) -> usize {
    let _ = layout_text(scratch, renderer, limits, s, format);
    (((scratch.min_bounds().height + EPSILON) / line_height) as usize).max(1)
}

/// The string actually handed to iced, or `None` when the text already fits.
fn resolve<Renderer: text::Renderer>(
    renderer: &Renderer,
    scratch: &mut Plain<Renderer::Paragraph>,
    content: &str,
    format: Format<Renderer::Font>,
    mode: Mode,
    width: f32,
) -> Option<String> {
    if !width.is_finite() || width <= 0.0 || content.is_empty() {
        return None;
    }

    let size = format.size.unwrap_or_else(|| renderer.default_size());
    let line_height = format.line_height.to_absolute(size).0;
    let lines = mode.lines();

    // Lay the whole string out exactly as it would be drawn.
    let mut wrapped = format;
    wrapped.width = Length::Shrink;
    wrapped.height = Length::Shrink;
    let limits = layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY));
    let _ = layout_text(scratch, renderer, &limits, content, wrapped);
    let bounds = scratch.min_bounds();
    if bounds.width <= width + EPSILON && bounds.height <= line_height * lines as f32 + EPSILON {
        return None;
    }

    // Where the last permitted line starts. `cosmic-text`'s `hit` only reports
    // the logical *start* of a visual line when the probe falls outside the
    // line's glyphs on the side that line starts from: `x < 0` for an LTR run,
    // `x` past the right edge for an RTL one. A probe on the wrong side,
    // including `x == 0` on a centre-aligned line, returns the line's logical
    // *end* instead, which is always the larger offset. So probe both sides and
    // take the smaller. `hit` reports a byte index within its own buffer line,
    // so this is only an absolute offset when the content has no hard breaks.
    let offset = if lines > 1 && format.wrapping != Wrapping::None && !content.contains('\n') {
        let y = (lines as f32 - 0.5) * line_height;
        let probe = |x: f32| {
            scratch
                .raw()
                .hit_test(Point::new(x, y))
                .map(Hit::cursor)
                .filter(|offset| *offset < content.len() && content.is_char_boundary(*offset))
        };
        probe(-1.0)
            .into_iter()
            .chain(probe(FAR_RIGHT))
            .min()
            .unwrap_or(0)
    } else {
        0
    };

    // For mixed-direction text a visual line's logical byte coverage is not
    // contiguous, so neither probe need land on its first byte. Walk the offset
    // back until what precedes it really does fit in the earlier lines.
    let mut offset = offset;
    if offset > 0
        && line_count(
            renderer,
            scratch,
            content[..offset].trim_end(),
            wrapped,
            &limits,
            line_height,
        ) >= lines
    {
        let candidates = boundaries(&content[..offset]);
        let (mut lo, mut hi) = (0, candidates.len() - 1);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let fits = line_count(
                renderer,
                scratch,
                content[..candidates[mid]].trim_end(),
                wrapped,
                &limits,
                line_height,
            ) < lines;
            if fits {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        offset = candidates[lo];
    }

    // Measure candidates with the real font, size and shaping, unwrapped.
    let mut flat = format;
    flat.width = Length::Shrink;
    flat.height = Length::Shrink;
    flat.wrapping = Wrapping::None;
    let infinite = layout::Limits::new(Size::ZERO, Size::INFINITE);
    let mut measure = |candidate: &str| {
        let _ = layout_text(scratch, renderer, &infinite, candidate, flat);
        scratch.min_bounds().width
    };

    let tail = single_line(&mut measure, &content[offset..], mode, width);
    if offset == 0 {
        return Some(tail);
    }

    // A hard break keeps the earlier lines wrapping exactly as they did.
    let composed = format!("{}\n{tail}", content[..offset].trim_end());

    // If anything above was off by a line, the ellipsis would be pushed onto a
    // line the caller clips away, which looks like no ellipsizing at all. Check,
    // and fall back to ellipsizing the whole string onto one line.
    let _ = layout_text(scratch, renderer, &limits, &composed, wrapped);
    if scratch.min_bounds().height <= line_height * lines as f32 + EPSILON {
        return Some(composed);
    }
    let mut measure = |candidate: &str| {
        let _ = layout_text(scratch, renderer, &infinite, candidate, flat);
        scratch.min_bounds().width
    };
    Some(single_line(&mut measure, content, mode, width))
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Ellipsize<'_, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::default())
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
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let width = limits.max().width;

        let fresh = match &state.cache {
            Some((cached_width, source, _)) => *cached_width != width || source != &*self.content,
            None => true,
        };
        if fresh {
            let resolved = resolve(
                renderer,
                &mut state.scratch,
                &self.content,
                self.format,
                self.mode,
                width,
            )
            .unwrap_or_else(|| self.content.to_string());
            state.cache = Some((width, self.content.to_string(), resolved));
        }

        let content = state.cache.as_ref().map_or("", |(_, _, resolved)| &**resolved);
        layout_text(&mut state.paragraph, renderer, limits, content, self.format)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        draw_text(
            renderer,
            defaults,
            layout.bounds(),
            state.paragraph.raw(),
            theme.style(&self.class),
            viewport,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Ellipsize<'a, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(text: Ellipsize<'a, Theme, Renderer>) -> Self {
        Element::new(text)
    }
}

/// Ellipsized text with the Body typography preset.
pub fn body<'a>(
    content: impl Into<Cow<'a, str>>,
    mode: Mode,
) -> Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
    Ellipsize::new(content, mode)
        .size(14.0)
        .line_height(LineHeight::Absolute(21.0.into()))
        .font(crate::ui::font::default())
}

/// Ellipsized text with the Heading typography preset.
pub fn heading<'a>(
    content: impl Into<Cow<'a, str>>,
    mode: Mode,
) -> Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
    Ellipsize::new(content, mode)
        .size(14.0)
        .line_height(LineHeight::Absolute(21.0.into()))
        .font(crate::ui::font::bold())
}

/// Ellipsized text with the default font, as `widget::text` builds it.
pub fn text<'a>(
    content: impl Into<Cow<'a, str>>,
    mode: Mode,
) -> Ellipsize<'a, crate::ui::Theme, crate::ui::Renderer> {
    Ellipsize::new(content, mode).font(crate::ui::font::default())
}

#[cfg(test)]
mod tests {
    use super::{ELLIPSIS, Mode, single_line};

    /// Every character one pixel wide, so the expected result can be written
    /// out by hand. Grapheme clustering is still the real thing.
    fn measure(s: &str) -> f32 {
        s.chars().filter(|c| *c != '\u{301}').count() as f32
    }

    #[test]
    fn end_keeps_a_head() {
        let out = single_line(&mut measure, "abcdefghij", Mode::End(1), 5.0);
        assert_eq!(out, format!("abcd{ELLIPSIS}"));
    }

    #[test]
    fn middle_keeps_a_head_and_a_tail() {
        // Head fills half the width, the tail whatever is left after `…`.
        let out = single_line(&mut measure, "abcdefghij", Mode::Middle(1), 6.0);
        assert_eq!(out, format!("abc{ELLIPSIS}ij"));
    }

    #[test]
    fn never_splits_a_grapheme_cluster() {
        // "e" + COMBINING ACUTE is one cluster and one pixel wide here.
        let content = "ae\u{301}be\u{301}ce\u{301}de\u{301}";
        let out = single_line(&mut measure, content, Mode::Middle(1), 4.0);
        assert!(!out.contains("\u{301}") || out.contains("e\u{301}"));
        for boundary in [0, out.len()] {
            assert!(out.is_char_boundary(boundary));
        }
        assert_eq!(out, format!("ae\u{301}{ELLIPSIS}e\u{301}"));
    }
}
