//! A container whose content's children keep their state by key rather
//! than by place.
//!
//! iced keeps a child's state by its position among its siblings. A list
//! that builds only the rows on screen puts a different file in each place
//! as it scrolls: the row coming into a place would take over the hover,
//! press and click state of the one that left it, and every row would have
//! its text laid out afresh. Before its content diffs its children, this
//! moves their states into the places their keys now have, so each keeps
//! its own wherever it is, and only a new key starts afresh.

use std::collections::HashMap;

use iced_core::event::Event;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Vector, Widget, layout, mouse, overlay,
    renderer,
};

/// `content`, whose children are keyed by `keys`, one per child in order.
pub fn keyed_children<'a, Message, Theme, Renderer>(
    keys: Vec<u64>,
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> KeyedChildren<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    KeyedChildren {
        keys,
        content: content.into(),
    }
}

#[allow(missing_debug_implementations)]
pub struct KeyedChildren<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    keys: Vec<u64>,
    content: Element<'a, Message, Theme, Renderer>,
}

/// The keys the content's child states are in place for.
struct Keys(Vec<u64>);

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for KeyedChildren<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Keys>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Keys(self.keys.clone()))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        let Tree {
            state, children, ..
        } = tree;
        let keys = &mut state.downcast_mut::<Keys>().0;
        if *keys != self.keys
            && let Some(content) = children.first_mut()
            && content.tag == self.content.as_widget().tag()
        {
            // Each state goes to where its key is now; a key that is new
            // gets an empty one, which the content's own diff fills in
            let mut by_key: HashMap<u64, Tree> = keys
                .iter()
                .copied()
                .zip(content.children.drain(..))
                .collect();
            content.children = self
                .keys
                .iter()
                .map(|key| by_key.remove(key).unwrap_or_else(Tree::empty))
                .collect();
            keys.clone_from(&self.keys);
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = node.size();
        layout::Node::with_children(size, vec![node])
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
        event: &Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
            cursor_position,
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
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor_position,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout.children().next().unwrap(),
            cursor_position,
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

impl<'a, Message, Theme, Renderer> From<KeyedChildren<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: 'a + iced_core::Renderer,
    Theme: 'a,
{
    fn from(keyed: KeyedChildren<'a, Message, Theme, Renderer>) -> Self {
        Element::new(keyed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Renderer = crate::ui::Renderer;
    type Theme = crate::ui::Theme;

    /// A row whose state records the key and the build that made it.
    struct Probe {
        key: u64,
        build: u32,
    }

    impl Widget<(), Theme, Renderer> for Probe {
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<(u64, u32)>()
        }

        fn state(&self) -> tree::State {
            tree::State::new((self.key, self.build))
        }

        fn size(&self) -> iced_core::Size<Length> {
            iced_core::Size::new(Length::Fixed(1.0), Length::Fixed(1.0))
        }

        fn layout(
            &mut self,
            _tree: &mut Tree,
            _renderer: &Renderer,
            _limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(iced_core::Size::new(1.0, 1.0))
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut Renderer,
            _theme: &Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: mouse::Cursor,
            _viewport: &Rectangle,
        ) {
        }
    }

    fn rows(keys: &[u64], build: u32) -> Element<'static, (), Theme, Renderer> {
        let mut column = iced::widget::Column::new();
        for &key in keys {
            column = column.push(Element::new(Probe { key, build }));
        }
        keyed_children(keys.to_vec(), column).into()
    }

    fn states(tree: &Tree) -> Vec<(u64, u32)> {
        tree.children[0]
            .children
            .iter()
            .map(|child| *child.state.downcast_ref::<(u64, u32)>())
            .collect()
    }

    /// Scrolled by one row, each row's state goes with it and only the new
    /// row starts afresh. By place, every row would keep the state of the
    /// one before it.
    #[test]
    fn a_row_keeps_its_state_wherever_it_lands() {
        let first = rows(&[1, 2, 3], 0);
        let mut tree = Tree::new(&first);
        assert_eq!(states(&tree), [(1, 0), (2, 0), (3, 0)]);

        rows(&[2, 3, 4], 1).as_widget().diff(&mut tree);
        assert_eq!(states(&tree), [(2, 0), (3, 0), (4, 1)]);

        // The same keys again: nothing moves, nothing is rebuilt
        rows(&[2, 3, 4], 2).as_widget().diff(&mut tree);
        assert_eq!(states(&tree), [(2, 0), (3, 0), (4, 1)]);
    }
}
