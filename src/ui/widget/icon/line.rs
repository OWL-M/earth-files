// SPDX-License-Identifier: GPL-3.0-only

//! The header's thin outline icons, drawn here rather than taken from the
//! icon theme, whose glyphs vary from theme to theme: 16 px, a 1.6 px round
//! stroke, as in the header mockups. [`handle`] makes them symbolic, so they
//! take the colour of what they sit on.

use super::{Handle, from_svg_bytes};

/// An outline icon of `shapes`, in the style above.
macro_rules! outline {
    ($shapes:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">"#,
            $shapes,
            "</svg>"
        )
        .as_bytes()
    };
}

/// Back in the history
pub const PREVIOUS: &[u8] = outline!(r#"<path d="M10 3 5 8l5 5"/>"#);
/// Forward in the history
pub const NEXT: &[u8] = outline!(r#"<path d="m6 3 5 5-5 5"/>"#);
/// The sidebar toggle while the sidebar is shown: the panel and its pane
pub const SIDEBAR_SHOWN: &[u8] =
    outline!(r#"<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M6 3v10"/>"#);
/// The sidebar toggle while the sidebar is hidden: the pane folded away to a
/// faint line at the edge
pub const SIDEBAR_HIDDEN: &[u8] = outline!(
    r#"<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M3.8 5.5v5" stroke-opacity=".55"/>"#
);
/// The menus, folded into one button
pub const MENU: &[u8] = outline!(r#"<path d="M3 4h10M3 8h10M3 12h10"/>"#);
/// Searching
pub const SEARCH: &[u8] =
    outline!(r#"<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/>"#);
/// Minimizing the window
pub const MINIMIZE: &[u8] = outline!(r#"<path d="M4 11h8"/>"#);
/// Maximizing the window
pub const MAXIMIZE: &[u8] = outline!(r#"<rect x="4" y="4" width="8" height="8" rx="1"/>"#);
/// Closing the window
pub const CLOSE: &[u8] = outline!(r#"<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>"#);

/// A symbolic icon of one of the outlines above.
pub fn handle(svg: &'static [u8]) -> Handle {
    from_svg_bytes(svg).symbolic(true)
}
