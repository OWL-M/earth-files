// Copyright 2022 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic d9431dc, src/widget/text.rs
//!
//! The `Text` widget itself is no longer iced's: [`widget`] carries the fork's
//! selectable `Text` (libcosmic's iced fork, `iced/core/src/widget/text.rs`),
//! which upstream has no counterpart for. The typography presets below build
//! that one, so `selectable_text` has a selectable widget to wrap and
//! `ui::widget::text(..)` behaves as it did under libcosmic.

use iced::Renderer;
use iced_core::text::LineHeight;

pub mod widget;
pub use widget::{Catalog, Format, Style, StyleFn, Text, draw, layout};
use std::borrow::Cow;

// `HasSelectableText` and `clipboard_has_text` are not from libcosmic's
// `src/widget/text.rs` but from its *iced fork*, `iced/core/src/widget/text.rs`
// (`:1054`, `:1062`), which upstream `iced_core 0.14` has no counterpart for at
// any layer. They are vendored into this module because it is this crate's
// `widget::text`, which is where the fork keeps them.
//
// The fork's own two impls read private, fork-only fields of iced's widget
// state, so neither could be written against iced's types from outside. Both
// are now implemented against types this crate owns instead: `widget::Text`
// below, and `ui::widget::text_editor::TextEditor`.
use iced_core::widget::tree::Tree as WidgetTree;
use iced_core::{Clipboard, Point};

/// Returns `true` if the clipboard currently holds non-empty text.
pub fn clipboard_has_text(clipboard: &dyn Clipboard) -> bool {
    clipboard
        .read(iced_core::clipboard::Kind::Standard)
        .is_some_and(|s| !s.is_empty())
}

/// Implement this on a widget to enable context menu support for
/// text selection (Copy, Select All, and optionally Cut / Paste) in libcosmic
pub trait HasSelectableText {
    /// Returns the currently selected text, if any.
    fn selected_text(&self, tree: &WidgetTree) -> Option<String>;

    /// Selects all text.
    fn select_all(&self, tree: &mut WidgetTree);

    /// Returns `true` if the widget is editable (shows Cut / Paste).
    fn is_editable(&self) -> bool {
        false
    }

    /// Returns `true` if the widget has any text (enables Select All).
    fn has_text(&self, _tree: &WidgetTree) -> bool {
        true
    }

    /// Returns whether the clipboard held text when the context menu was
    /// requested (enables Paste). Widgets cache this on right-click because
    /// overlays have no clipboard access.
    fn clipboard_has_text(&self, _tree: &WidgetTree) -> bool {
        true
    }

    /// Returns `true` if the widget is currently focused.
    fn is_focused(&self, tree: &WidgetTree) -> bool;

    /// Returns the position where the context menu should appear
    fn context_menu_position(&self, tree: &WidgetTree) -> Option<Point>;

    /// Sets or clears the context menu position.
    fn set_context_menu_position(&self, tree: &mut WidgetTree, pos: Option<Point>);

    /// Copies the selection to the clipboard.
    fn copy_to_clipboard(&self, tree: &WidgetTree, clipboard: &mut dyn Clipboard) {
        if let Some(text) = self.selected_text(tree) {
            clipboard.write(iced_core::clipboard::Kind::Standard, text);
        }
    }

    /// Deletes the selected text and returns the new full content.
    /// Only called when [`is_editable`](Self::is_editable) is `true`.
    fn delete_selection(&self, _tree: &mut WidgetTree) -> Option<String> {
        None
    }

    /// Inserts `text` at the cursor (replacing any selection) and returns
    /// the new full content.
    /// Only called when [`is_editable`](Self::is_editable) is `true`.
    fn paste_text(&self, _tree: &mut WidgetTree, _text: &str) -> Option<String> {
        None
    }
}


/// Creates a new [`Text`] widget with the provided content.
///
/// [`Text`]: widget::Text
pub fn text<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    Text::new(text.into()).font(crate::ui::font::default())
}

/// Available presets for text typography
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Typography {
    Body,
    Caption,
    CaptionHeading,
    Heading,
    Monotext,
    Title1,
    Title2,
    Title3,
    Title4,
}

/// [`Text`] widget with the Title 1 typography preset.
pub fn title1<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(35.0)
            .line_height(LineHeight::Absolute(52.0.into()))
            .font(crate::ui::font::semibold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Title 2 typography preset.
pub fn title2<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(29.0)
            .line_height(LineHeight::Absolute(43.0.into()))
            .font(crate::ui::font::semibold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Title 3 typography preset.
pub fn title3<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(24.0)
            .line_height(LineHeight::Absolute(36.0.into()))
            .font(crate::ui::font::bold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Title 4 typography preset.
pub fn title4<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(20.0)
            .line_height(LineHeight::Absolute(30.0.into()))
            .font(crate::ui::font::bold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Heading typography preset.
pub fn heading<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(14.0)
            .line_height(LineHeight::Absolute(iced::Pixels(21.0)))
            .font(crate::ui::font::bold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Caption Heading typography preset.
pub fn caption_heading<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(12.0)
            .line_height(LineHeight::Absolute(iced::Pixels(17.0)))
            .font(crate::ui::font::semibold())
    }

    inner(text.into())
}

/// [`Text`] widget with the Body typography preset.
pub fn body<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(14.0)
            .line_height(LineHeight::Absolute(21.0.into()))
            .font(crate::ui::font::default())
    }

    inner(text.into())
}

/// [`Text`] widget with the Caption typography preset.
pub fn caption<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(12.0)
            .line_height(LineHeight::Absolute(17.0.into()))
            .font(crate::ui::font::default())
    }

    inner(text.into())
}

/// [`Text`] widget with the Monotext typography preset.
pub fn monotext<'a>(text: impl Into<Cow<'a, str>> + 'a) -> Text<'a, crate::ui::Theme, Renderer> {
    #[inline(never)]
    fn inner(text: Cow<str>) -> Text<crate::ui::Theme, Renderer> {
        Text::new(text)
            .size(14.0)
            .line_height(LineHeight::Absolute(20.0.into()))
            .font(crate::ui::font::mono())
    }

    inner(text.into())
}


// ---------------------------------------------------------------------------
// `HasSelectableText` for the vendored `Text`.
//
// Ported from the fork's own impl (`iced/core/src/widget/text.rs:1129`), which
// could not be written against iced's `Text` because it reads
// `State::selection`, a fork-only private field. `widget::State` is this
// crate's, so the impl is a straight copy.
// ---------------------------------------------------------------------------

impl<Theme: Catalog> HasSelectableText for Text<'_, Theme, Renderer> {
    fn selected_text(&self, tree: &WidgetTree) -> Option<String> {
        let state = tree.state.downcast_ref::<widget::State>();
        let (left, right) = state.selection_range()?;
        let content = state.paragraph.content();
        let lo = widget::grapheme_to_byte(content, left);
        let hi = widget::grapheme_to_byte(content, right);
        content.get(lo..hi).map(std::borrow::ToOwned::to_owned)
    }

    fn select_all(&self, tree: &mut WidgetTree) {
        let state = tree.state.downcast_mut::<widget::State>();
        state.select_all();
    }

    fn has_text(&self, tree: &WidgetTree) -> bool {
        let state = tree.state.downcast_ref::<widget::State>();
        !state.paragraph.content().is_empty()
    }

    fn clipboard_has_text(&self, tree: &WidgetTree) -> bool {
        tree.state.downcast_ref::<widget::State>().has_clipboard_text()
    }

    fn is_focused(&self, tree: &WidgetTree) -> bool {
        tree.state.downcast_ref::<widget::State>().is_focused()
    }

    fn context_menu_position(&self, tree: &WidgetTree) -> Option<Point> {
        tree.state
            .downcast_ref::<widget::State>()
            .context_menu_position()
    }

    fn set_context_menu_position(&self, tree: &mut WidgetTree, pos: Option<Point>) {
        tree.state
            .downcast_mut::<widget::State>()
            .set_context_menu_position(pos);
    }
}
