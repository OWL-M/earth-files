// SPDX-License-Identifier: GPL-3.0-only

//! Shows content from some way down, without moving it.
//!
//! A context menu's popup keeps room above the menu for a group to unfold
//! into, and the menu has to collapse into its own corner rather than that
//! room's. So what wraps the menu — a texture cache, and the genie on it —
//! must see only the part from the menu's top down, while the menu itself is
//! laid out and placed exactly as it would be without any of this.
//!
//! Two halves do that, around whatever sees the cropped part:
//! [`Top::place`] puts it `top` down in the surface, and [`Top::crop`] puts
//! the content back up by as much, so the content lands where it was. Both
//! read `top` when they are laid out, not when the view is built: a group
//! unfolds over frames that lay the popup out again without building its
//! view again, and an offset read once would cut the rows moving up off.
//! And both read it once their content is laid out, which can change it:
//! `place` is outermost, so the two read the same, final value.

use std::rc::Rc;

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Clipboard, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, layout, mouse,
    overlay, renderer,
};

use crate::ui::Element;

/// How far down the content begins, read at each layout.
#[derive(Clone)]
pub struct Top(Rc<dyn Fn() -> f32>);

impl Top {
    pub fn new(read: impl Fn() -> f32 + 'static) -> Self {
        Self(Rc::new(read))
    }

    fn get(&self) -> f32 {
        (self.0)().max(0.0)
    }

    /// Puts `content` `top` down in what it is given, which keeps the full
    /// size.
    pub fn place<'a, Message>(
        &self,
        content: impl Into<Element<'a, Message>>,
    ) -> CropTop<'a, Message> {
        CropTop {
            top: self.clone(),
            place: true,
            content: content.into(),
        }
    }

    /// Bounds `content` from `top` down, placing it back up by `top`.
    pub fn crop<'a, Message>(
        &self,
        content: impl Into<Element<'a, Message>>,
    ) -> CropTop<'a, Message> {
        CropTop {
            top: self.clone(),
            place: false,
            content: content.into(),
        }
    }
}

/// One half of a crop: see [`Top`].
#[allow(missing_debug_implementations)]
pub struct CropTop<'a, Message> {
    top: Top,
    /// [`Top::place`] rather than [`Top::crop`].
    place: bool,
    content: Element<'a, Message>,
}

impl<Message> Widget<Message, crate::ui::Theme, crate::ui::Renderer> for CropTop<'_, Message> {
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
        renderer: &crate::ui::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The content is laid out with the whole room, as without any of
        // this, and `top` is read after: laying out can change it, as a menu
        // given less height than it asked for gives up its room to its rows
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let top = self.top.get();
        // Placing puts the cropped part down by `top`; cropping puts the
        // content back up
        let shift = if self.place { top } else { -top };
        let size = node.size();
        layout::Node::with_children(
            Size::new(size.width, (size.height + shift).max(0.0)),
            vec![node.move_to(Point::new(0.0, shift))],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &crate::ui::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().unwrap(),
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
        renderer: &crate::ui::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
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
        renderer: &crate::ui::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut crate::ui::Renderer,
        theme: &crate::ui::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &crate::ui::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, crate::ui::Theme, crate::ui::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<CropTop<'a, Message>> for Element<'a, Message> {
    fn from(crop: CropTop<'a, Message>) -> Element<'a, Message> {
        Element::new(crop)
    }
}

#[cfg(test)]
mod tests {
    use super::Top;
    use crate::ui::widget::{measure_bounds, space};
    use crate::ui::{Element, Theme};
    use iced_core::{Length, Point, Rectangle, Size, mouse, renderer};
    use iced_runtime::user_interface::{Cache, UserInterface};
    use std::cell::Cell;
    use std::rc::Rc;

    /// The content stays where it was, the crop's bounds are the part below
    /// `top`, and a `top` that changes is followed by the next layout,
    /// without the view being built again.
    #[test]
    fn the_crop_follows_its_top_and_the_content_stays_put() {
        let (content, crop) = (
            Cell::new(Rectangle::default()),
            Cell::new(Rectangle::default()),
        );
        let at = Rc::new(Cell::new(64.0));
        let read = at.clone();
        let top = Top::new(move || read.get());
        let view: Element<'_, ()> = top
            .place(measure_bounds(
                top.crop(measure_bounds(
                    space::vertical()
                        .width(Length::Fixed(100.0))
                        .height(Length::Fixed(200.0)),
                    &content,
                )),
                &crop,
            ))
            .into();
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let size = Size::new(100.0, 200.0);
        let mut ui = UserInterface::build(view, size, Cache::default(), &mut renderer);
        let draw = |ui: &mut UserInterface<'_, (), Theme, crate::ui::Renderer>,
                    renderer: &mut crate::ui::Renderer| {
            ui.draw(
                renderer,
                &Theme::default(),
                &renderer::Style::default(),
                mouse::Cursor::Unavailable,
            );
        };
        draw(&mut ui, &mut renderer);
        let whole = Rectangle::new(Point::ORIGIN, size);
        assert_eq!(content.get(), whole);
        assert_eq!(
            crop.get(),
            Rectangle::new(Point::new(0.0, 64.0), Size::new(100.0, 136.0))
        );

        at.set(16.0);
        let mut ui = ui.relayout(size, &mut renderer);
        draw(&mut ui, &mut renderer);
        assert_eq!(content.get(), whole);
        assert_eq!(
            crop.get(),
            Rectangle::new(Point::new(0.0, 16.0), Size::new(100.0, 184.0))
        );
    }
}
