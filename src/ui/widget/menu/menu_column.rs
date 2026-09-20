// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Vendored from pop-os/libcosmic, src/widget/menu/menu_column.rs
//!
//! Distribute content vertically.
use iced_core::alignment::{self, Alignment};
use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Padding, Pixels, Rectangle, Shell, Size, Vector, Widget,
    layout, mouse, overlay, renderer,
};

#[allow(missing_debug_implementations)]
#[must_use]
pub struct MenuColumn<'a, Message, Renderer = iced::Renderer> {
    spacing: f32,
    padding: Padding,
    width: Length,
    height: Length,
    max_width: f32,
    align: Alignment,
    clip: bool,
    children: Vec<Element<'a, Message, crate::ui::Theme, Renderer>>,
}

impl<'a, Message, Renderer> MenuColumn<'a, Message, Renderer>
where
    Renderer: iced_core::Renderer,
{
    /// Creates an empty [`MenuColumn`].
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    /// Creates a [`MenuColumn`] with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(Vec::with_capacity(capacity))
    }

    /// Creates a [`MenuColumn`] with the given elements.
    pub fn with_children(
        children: impl IntoIterator<Item = Element<'a, Message, crate::ui::Theme, Renderer>>,
    ) -> Self {
        let iterator = children.into_iter();

        Self::with_capacity(iterator.size_hint().0).extend(iterator)
    }

    /// Creates a [`MenuColumn`] from an already allocated [`Vec`].
    ///
    /// Keep in mind that the [`MenuColumn`] will not inspect the [`Vec`], which means
    /// it won't automatically adapt to the sizing strategy of its contents.
    ///
    /// If any of the children have a [`Length::Fill`] strategy, you will need to
    /// call [`MenuColumn::width`] or [`MenuColumn::height`] accordingly.
    pub fn from_vec(children: Vec<Element<'a, Message, crate::ui::Theme, Renderer>>) -> Self {
        Self {
            spacing: 0.0,
            padding: Padding::ZERO,
            width: Length::Shrink,
            height: Length::Shrink,
            max_width: f32::INFINITY,
            align: Alignment::Start,
            clip: false,
            children,
        }
    }

    /// Sets the vertical spacing _between_ elements.
    ///
    /// Custom margins per element do not exist in iced. You should use this
    /// method instead! While less flexible, it helps you keep spacing between
    /// elements consistent.
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into().0;
        self
    }

    /// Sets the [`Padding`] of the [`MenuColumn`].
    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the width of the [`MenuColumn`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`MenuColumn`].
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the maximum width of the [`MenuColumn`].
    pub fn max_width(mut self, max_width: impl Into<Pixels>) -> Self {
        self.max_width = max_width.into().0;
        self
    }

    /// Sets the horizontal alignment of the contents of the [`MenuColumn`] .
    pub fn align_x(mut self, align: impl Into<alignment::Horizontal>) -> Self {
        self.align = Alignment::from(align.into());
        self
    }

    /// Sets whether the contents of the [`MenuColumn`] should be clipped on
    /// overflow.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    /// Adds an element to the [`MenuColumn`].
    pub fn push(
        mut self,
        child: impl Into<Element<'a, Message, crate::ui::Theme, Renderer>>,
    ) -> Self {
        let child = child.into();
        let child_size = child.as_widget().size_hint();

        self.width = self.width.enclose(child_size.width);
        self.height = self.height.enclose(child_size.height);

        self.children.push(child);
        self
    }

    /// Adds an element to the [`MenuColumn`], if `Some`.
    pub fn push_maybe(
        self,
        child: Option<impl Into<Element<'a, Message, crate::ui::Theme, Renderer>>>,
    ) -> Self {
        if let Some(child) = child {
            self.push(child)
        } else {
            self
        }
    }

    /// Extends the [`MenuColumn`] with the given children.
    pub fn extend(
        self,
        children: impl IntoIterator<Item = Element<'a, Message, crate::ui::Theme, Renderer>>,
    ) -> Self {
        children.into_iter().fold(self, Self::push)
    }
}

impl<Message, Renderer> Default for MenuColumn<'_, Message, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, Message, Renderer: iced_core::Renderer>
    FromIterator<Element<'a, Message, crate::ui::Theme, Renderer>>
    for MenuColumn<'a, Message, Renderer>
{
    fn from_iter<T: IntoIterator<Item = Element<'a, Message, crate::ui::Theme, Renderer>>>(
        iter: T,
    ) -> Self {
        Self::with_children(iter)
    }
}

impl<Message, Renderer> Widget<Message, crate::ui::Theme, Renderer>
    for MenuColumn<'_, Message, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(self.children.as_slice());
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.max_width(self.max_width);

        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            &limits,
            self.width,
            self.height,
            self.padding,
            self.spacing,
            self.align,
            &mut self.children,
            &mut tree.children,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), c_layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, c_layout, renderer, operation);
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
        for (((i, child), state), c_layout) in self
            .children
            .iter_mut()
            .enumerate()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, c_layout, cursor, renderer, clipboard, shell, viewport,
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
                child
                    .as_widget()
                    .mouse_interaction(state, c_layout, cursor, viewport, renderer)
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
        if let Some(clipped_viewport) = layout.bounds().intersection(viewport) {
            let viewport = if self.clip {
                &clipped_viewport
            } else {
                viewport
            };

            for (i, ((child, state), c_layout)) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
                .filter(|(_, layout)| layout.bounds().intersects(viewport))
                .enumerate()
            {
                let t = theme.with_list_item_position(if self.children.len() == 1 {
                    Some((Alignment::Center, i))
                } else if 0 == i {
                    Some((Alignment::Start, i))
                } else if i == self.children.len() - 1 {
                    Some((Alignment::End, i))
                } else {
                    None
                });
                child
                    .as_widget()
                    .draw(state, renderer, &t, style, c_layout, cursor, viewport);
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
}

impl<'a, Message, Renderer> From<MenuColumn<'a, Message, Renderer>>
    for Element<'a, Message, crate::ui::Theme, Renderer>
where
    Message: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(column: MenuColumn<'a, Message, Renderer>) -> Self {
        Self::new(column)
    }
}
