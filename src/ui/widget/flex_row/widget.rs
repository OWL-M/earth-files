// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic d9431dc, src/widget/flex_row/widget.rs

use iced::Renderer;

use crate::ui::Element;
use derive_setters::Setters;
use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Layout, Length, Padding, Rectangle, Shell, Vector, Widget, layout, mouse, overlay,
    renderer,
};

/// Responsively generates rows and columns of widgets based on its dimensions.
#[derive(Setters)]
#[must_use]
pub struct FlexRow<'a, Message> {
    #[setters(skip)]
    children: Vec<Element<'a, Message>>,
    /// Sets the padding around the widget.
    #[setters(into)]
    padding: Padding,
    /// Sets the space between each column of items.
    column_spacing: u16,
    /// Sets the space between each item in a row.
    row_spacing: u16,
    /// Sets the width.
    width: Length,
    /// Sets minimum width of items that grow.
    #[setters(into)]
    min_item_width: Option<f32>,
    /// Sets the max width
    max_width: f32,
    /// Defines how content will be aligned horizontally.
    #[setters(skip)]
    align_items: Option<taffy::AlignItems>,
    /// Defines how content will be aligned vertically.
    #[setters(skip)]
    justify_items: Option<taffy::AlignItems>,
    /// Defines how the content will be justified.
    #[setters(into)]
    justify_content: Option<taffy::JustifyContent>,
}

impl<'a, Message> FlexRow<'a, Message> {
    pub(crate) const fn new(children: Vec<Element<'a, Message>>) -> Self {
        Self {
            children,
            padding: Padding::ZERO,
            column_spacing: 4,
            row_spacing: 4,
            width: Length::Shrink,
            min_item_width: None,
            max_width: f32::INFINITY,
            align_items: None,
            justify_items: None,
            justify_content: None,
        }
    }

    /// Defines how content will be aligned horizontally.
    pub fn align_items(mut self, alignment: iced::Alignment) -> Self {
        self.align_items = Some(match alignment {
            iced::Alignment::Center => taffy::AlignItems::CENTER,
            iced::Alignment::Start => taffy::AlignItems::START,
            iced::Alignment::End => taffy::AlignItems::END,
        });
        self
    }

    /// Defines how content will be aligned vertically.
    pub fn justify_items(mut self, alignment: iced::Alignment) -> Self {
        self.justify_items = Some(match alignment {
            iced::Alignment::Center => taffy::AlignItems::CENTER,
            iced::Alignment::Start => taffy::AlignItems::START,
            iced::Alignment::End => taffy::AlignItems::END,
        });
        self
    }

    /// Sets the space between each column and row.
    #[inline]
    pub const fn spacing(mut self, spacing: u16) -> Self {
        self.column_spacing = spacing;
        self.row_spacing = spacing;
        self
    }
}

impl<Message: 'static + Clone> Widget<Message, crate::ui::Theme, Renderer> for FlexRow<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(self.children.as_slice());
    }

    fn size(&self) -> iced_core::Size<Length> {
        iced_core::Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = self.size();
        let limits = limits
            .max_width(self.max_width)
            .width(size.width)
            .height(size.height);

        super::layout::resolve(
            renderer,
            &limits,
            &mut self.children,
            self.padding,
            f32::from(self.column_spacing),
            f32::from(self.row_spacing),
            self.min_item_width,
            self.justify_items,
            self.align_items,
            self.justify_content,
            &mut tree.children,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), c_layout)| {
                    child.as_widget_mut().operate(
                        state,
                        c_layout,
                        renderer,
                        operation,
                    );
                });
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
        for ((child, state), c_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state,
                event,
                c_layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), c_layout)| {
                child.as_widget().mouse_interaction(
                    state,
                    c_layout,
                    cursor,
                    viewport,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), c_layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child.as_widget().draw(
                state,
                renderer,
                theme,
                style,
                c_layout,
                cursor,
                viewport,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }

    // TODO(dnd): removed the `Widget::drag_destinations` router (upstream iced 0.14
    // has no such method). Tab drag-to-reorder must be rebuilt on smithay-clipboard;
    // without this pass-through the tab bar leaf is unreachable from the root walk.
}

impl<'a, Message: 'static + Clone> From<FlexRow<'a, Message>> for Element<'a, Message> {
    fn from(flex_row: FlexRow<'a, Message>) -> Self {
        Self::new(flex_row)
    }
}
