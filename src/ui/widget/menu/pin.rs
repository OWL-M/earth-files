// SPDX-License-Identifier: GPL-3.0-only

//! The pin at the end of a menu row: the shape of a group row's count, lit
//! with its row and again when the pointer is on the pin itself.

use iced::Color;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Background, Border, Clipboard, Element, Layout, Length, Rectangle, Shadow, Shell, Size, Vector,
    layout, mouse, overlay, renderer,
};

use super::unfold;
use crate::ui::convert::ToColor;
use crate::ui::{Renderer, Theme};

/// How wide the pill is: as wide as a count at its narrowest.
pub const WIDTH: f32 = unfold::CHIP_MIN;
/// The pin inside it.
pub const ICON: u16 = 14;

/// Paints the pill behind `content`, which takes the presses.
pub fn pill<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Element<'a, Message, Theme, Renderer> {
    Element::new(Pill {
        content: content.into(),
    })
}

/// How far a pin's pill is lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lit {
    /// As a count at rest.
    Rest,
    /// Its row is lit.
    Row,
    /// The pointer is on the pin.
    Pin,
}

impl Lit {
    /// A menu gives a row's content the pointer only while it is over that
    /// row, or a stand-in on a row that stays lit, so having one at all says
    /// the row is lit.
    fn of(cursor: mouse::Cursor, bounds: Rectangle) -> Self {
        if cursor.is_over(bounds) {
            Self::Pin
        } else if cursor.position().is_some() {
            Self::Row
        } else {
            Self::Rest
        }
    }

    /// At rest and on the pin it is what a count is at rest and hovered.
    fn fill(self, theme: &Theme) -> Color {
        let cosmic = theme.cosmic();
        let (mut fill, alpha) = match self {
            Self::Rest => (cosmic.control_5(), 0.2),
            Self::Row => (cosmic.control_5(), 0.45),
            Self::Pin => (cosmic.accent_color(), 0.15),
        };
        fill.alpha = alpha;
        fill.to_color()
    }
}

struct Pill<'a, Message> {
    content: Element<'a, Message, Theme, Renderer>,
}

impl<Message> iced_core::Widget<Message, Theme, Renderer> for Pill<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
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
        let content = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(content.size(), vec![content])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced_core::Event,
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
        renderer: &Renderer,
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
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced_core::Renderer as _;
        let bounds = layout.bounds();
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: (bounds.height / 2.0).into(),
                    ..Border::default()
                },
                shadow: Shadow::default(),
                snap: true,
            },
            Background::Color(Lit::of(cursor, bounds).fill(theme)),
        );
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
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Lit;
    use iced_core::{Point, Rectangle, Size, mouse};

    /// At rest, with its row, and again on the pin itself
    #[test]
    fn a_pin_lights_with_its_row_and_again_under_the_pointer() {
        let pin = Rectangle::new(Point::new(100.0, 10.0), Size::new(28.0, 20.0));
        assert_eq!(Lit::of(mouse::Cursor::Unavailable, pin), Lit::Rest);
        assert_eq!(
            Lit::of(mouse::Cursor::Available(Point::new(20.0, 20.0)), pin),
            Lit::Row
        );
        assert_eq!(
            Lit::of(mouse::Cursor::Available(pin.center()), pin),
            Lit::Pin
        );
    }

    #[test]
    fn each_of_the_three_looks_different() {
        let theme = crate::ui::Theme::default();
        let (rest, row, pin) = (
            Lit::Rest.fill(&theme),
            Lit::Row.fill(&theme),
            Lit::Pin.fill(&theme),
        );
        assert_ne!(rest, row);
        assert_ne!(row, pin);
        assert_ne!(rest, pin);
    }
}
