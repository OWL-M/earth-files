// Copyright 2019 Héctor Ramón, Iced contributors
// Copyright 2018 Jeremy Soller
// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Paragraph queries that iced's `Paragraph` trait lacks.
//!
//! `iced_core`/`iced_graphics` 0.14 carry no BiDi cursor affinity through the
//! text stack: `Hit::CharOffset` carries only the byte index, and `Paragraph`
//! has no `cursor_position(line, byte_index, affinity)` and no `highlight`
//! (which is what draws a selection rectangle, and which `ui::widget::text`
//! needs).
//!
//! They are free functions rather than a `Paragraph` trait, because the
//! widgets that use them are already monomorphic in `crate::ui::Renderer`, so its
//! associated `Paragraph` is the concrete `iced_graphics::text::Paragraph`,
//! whose `buffer()` accessor is public and hands back the
//! `cosmic_text::Buffer`.
//!
//! `cosmic_text::Affinity` is used directly (same two variants, `Before`
//! default); `cosmic-text` is a direct dependency.
//!
//! This graph resolves cosmic-text 0.15, whose cursor-glyph lookup
//! (`src/edit/editor.rs:33 cursor_glyph_opt`) ignores affinity entirely;
//! cosmic-text 0.19's cursor positioning is affinity-aware. The functions pass
//! affinity through, but it has no effect on the result until cosmic-text
//! moves to 0.19. Only this module will need to change then.

use cosmic_text::Affinity;
use iced_core::{Point, Rectangle};
use unicode_segmentation::UnicodeSegmentation;

/// The concrete paragraph behind `crate::ui::Renderer`.
pub type Paragraph = <crate::ui::Renderer as iced_core::text::Renderer>::Paragraph;

/// Hit-tests `paragraph`, returning the byte index *and* the cursor affinity,
/// which iced's `Hit` has nowhere to put.
pub fn hit_test_with_affinity(paragraph: &Paragraph, point: Point) -> Option<(usize, Affinity)> {
    let cursor = paragraph.buffer().hit(point.x, point.y)?;

    Some((cursor.index, cursor.affinity))
}

/// Returns the position of the cursor at `byte_index` on `line`.
///
/// cosmic-text 0.15 has `cursor_position` only on `Editor`, not on `Buffer`.
/// The body below is cosmic-text 0.15's own `Editor::cursor_position`
/// (`src/edit/editor.rs:912`) with its private
/// `cursor_position`/`cursor_glyph_opt` helpers inlined, so the arithmetic is
/// the same one an `Editor` over this buffer would perform.
pub fn cursor_position(
    paragraph: &Paragraph,
    line: usize,
    byte_index: usize,
    affinity: Affinity,
) -> Option<Point> {
    let cursor = cosmic_text::Cursor::new_with_affinity(line, byte_index, affinity);

    paragraph.buffer().layout_runs().find_map(|run| {
        let (cursor_glyph, cursor_glyph_offset) = cursor_glyph_opt(&cursor, &run)?;

        let x = run.glyphs.get(cursor_glyph).map_or_else(
            || {
                run.glyphs.last().map_or(0.0, |glyph| {
                    if glyph.level.is_rtl() {
                        glyph.x
                    } else {
                        glyph.x + glyph.w
                    }
                })
            },
            |glyph| {
                if glyph.level.is_rtl() {
                    glyph.x + glyph.w - cursor_glyph_offset
                } else {
                    glyph.x + cursor_glyph_offset
                }
            },
        );

        Some(Point::new(x, run.line_top))
    })
}

/// cosmic-text 0.15 `src/edit/editor.rs:33`, verbatim.
fn cursor_glyph_opt(
    cursor: &cosmic_text::Cursor,
    run: &cosmic_text::LayoutRun,
) -> Option<(usize, f32)> {
    if cursor.line == run.line_i {
        for (glyph_i, glyph) in run.glyphs.iter().enumerate() {
            if cursor.index == glyph.start {
                return Some((glyph_i, 0.0));
            } else if cursor.index > glyph.start && cursor.index < glyph.end {
                // Guess x offset based on characters
                let mut before = 0;
                let mut total = 0;

                let cluster = &run.text[glyph.start..glyph.end];
                for (i, _) in cluster.grapheme_indices(true) {
                    if glyph.start + i < cursor.index {
                        before += 1;
                    }
                    total += 1;
                }

                let offset = glyph.w * (before as f32) / (total as f32);
                return Some((glyph_i, offset));
            }
        }
        match run.glyphs.last() {
            Some(glyph) => {
                if cursor.index == glyph.end {
                    return Some((run.glyphs.len(), 0.0));
                }
            }
            None => {
                return Some((0, 0.0));
            }
        }
    }
    None
}

/// Returns the rectangles covering the selection from `start` to `end` on
/// `line`.
///
/// Caveat, for the same reason as `cursor_position` above: cosmic-text 0.19's
/// `LayoutRun::highlight` yields an *iterator* of `(x, width)` pairs, so a
/// selection crossing a BiDi direction change draws as several rectangles.
/// cosmic-text 0.15's returns a single `Option<(f32, f32)>`, so such a
/// selection draws as one merged rectangle spanning the run.
/// Selections within a single direction (every filename, path and timestamp
/// this app puts in the details pane) are identical either way.
pub fn highlight(
    paragraph: &Paragraph,
    line: usize,
    start: (usize, Affinity),
    end: (usize, Affinity),
) -> Vec<Rectangle> {
    let start_cursor = cosmic_text::Cursor::new_with_affinity(line, start.0, start.1);
    let end_cursor = cosmic_text::Cursor::new_with_affinity(line, end.0, end.1);

    paragraph
        .buffer()
        .layout_runs()
        .filter(|run| run.line_i == line)
        .flat_map(|run| {
            let line_top = run.line_top;
            let line_height = run.line_height;
            run.highlight(start_cursor, end_cursor)
                .into_iter()
                .filter(|(_, width)| *width > 0.0)
                .map(move |(x, w)| Rectangle {
                    x,
                    y: line_top,
                    width: w,
                    height: line_height,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether the paragraph direction of `line` is right-to-left.
///
/// `Buffer::is_rtl` arrived in cosmic-text 0.19 and does not exist on the 0.15
/// `Buffer` this graph resolves, so the answer is read off the line's own
/// layout run instead: `LayoutRun::rtl` is documented as "true if the original
/// paragraph direction is RTL" (cosmic-text 0.15 `src/buffer.rs:29`) and is
/// filled from `ShapeLine::rtl` (`:151`), which is the same quantity 0.19
/// exposes.
///
/// Returns `None` when the line has not been laid out. Every call site in this
/// crate treats that as "not RTL".
#[must_use]
pub fn is_rtl(paragraph: &Paragraph, line: usize) -> Option<bool> {
    paragraph
        .buffer()
        .layout_runs()
        .find(|run| run.line_i == line)
        .map(|run| run.rtl)
}
