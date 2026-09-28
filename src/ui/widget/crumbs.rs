// SPDX-License-Identifier: GPL-3.0-only

//! A row of breadcrumbs that shortens itself to the width it is given.
//!
//! [`Crumbs`] holds an icon, the crumbs from the outermost folder to the
//! current one, a separator before each crumb but the first, and one "more"
//! element per ancestor, standing for that ancestor and every crumb before
//! it. Laid out with room for everything, it shows the icon and every crumb.
//! With less, it hides the leading crumbs one by one and shows, in their
//! place, the "more" of the last one hidden, until the rest fits. The current
//! folder always stays; when it alone does not fit, it is laid out again in
//! the width left, where an ellipsizing crumb shortens itself.
//!
//! Hidden children get empty layout nodes and are skipped when drawing,
//! handling events, choosing the cursor, operating and finding overlays. What
//! every child sends is fixed when the view is built.

use iced_core::event::Event;
use iced_core::widget::Operation;
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Alignment, Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Vector, Widget, layout,
    mouse, overlay, renderer,
};

use crate::ui::{Renderer, Theme};

/// Builds a [`Crumbs`]. `crumbs` runs from the outermost folder to the
/// current one; `separators` has one fewer, the one at `i` standing before
/// `crumbs[i + 1]`; `more` has one fewer too, the one at `i` standing for
/// `crumbs[..=i]` when they are hidden.
///
/// Every child is first laid out with unlimited width, so children must be
/// `Length::Shrink` wide: a `Fill` child would be infinitely wide there and
/// nothing would ever fit.
///
/// # Panics
///
/// If `crumbs` is empty, or `separators` or `more` is not one shorter.
pub fn crumbs<'a, Message>(
    icon: impl Into<Element<'a, Message, Theme, Renderer>>,
    crumbs: Vec<Element<'a, Message, Theme, Renderer>>,
    separators: Vec<Element<'a, Message, Theme, Renderer>>,
    more: Vec<Element<'a, Message, Theme, Renderer>>,
) -> Crumbs<'a, Message> {
    let n = crumbs.len();
    assert!(n > 0, "there is always a current folder");
    assert_eq!(
        separators.len(),
        n - 1,
        "one separator before each crumb but the first"
    );
    assert_eq!(more.len(), n - 1, "one \"more\" per ancestor");
    let mut children = Vec::with_capacity(3 * n - 1);
    children.push(icon.into());
    children.extend(crumbs);
    children.extend(separators);
    children.extend(more);
    Crumbs {
        children,
        n,
        spacing: 0.0,
        invisible: false,
    }
}

/// See the [module documentation](self).
#[allow(missing_debug_implementations)]
pub struct Crumbs<'a, Message> {
    /// The icon, then the `n` crumbs, the `n - 1` separators and the `n - 1`
    /// "more" elements.
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    n: usize,
    spacing: f32,
    /// Laid out, but neither drawn nor given events; see [`Self::invisible`].
    invisible: bool,
}

/// How many leading crumbs the last layout hid.
#[derive(Debug, Default)]
struct State {
    hidden: usize,
}

impl<Message> Crumbs<'_, Message> {
    /// The gap between neighbouring children.
    #[must_use]
    pub const fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }

    /// Only takes up its room: nothing of it is drawn, and it takes no
    /// events. The header lays these out under the path field, so the field
    /// is as wide as the crumbs it stands in for.
    #[must_use]
    pub const fn invisible(mut self) -> Self {
        self.invisible = true;
        self
    }

    /// The children drawn and given events: those shown, none while
    /// invisible.
    fn live(&self, hidden: usize) -> Vec<usize> {
        if self.invisible {
            Vec::new()
        } else {
            self.shown(hidden)
        }
    }

    const fn crumb(&self, i: usize) -> usize {
        1 + i
    }

    const fn separator(&self, i: usize) -> usize {
        1 + self.n + i
    }

    const fn more(&self, i: usize) -> usize {
        1 + 2 * self.n - 1 + i
    }

    /// The children shown with `hidden` leading crumbs hidden, left to right.
    fn shown(&self, hidden: usize) -> Vec<usize> {
        let mut shown = vec![0];
        if hidden > 0 {
            shown.push(self.more(hidden - 1));
            shown.push(self.separator(hidden - 1));
        }
        for i in hidden..self.n {
            if i > hidden {
                shown.push(self.separator(i - 1));
            }
            shown.push(self.crumb(i));
        }
        shown
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Crumbs<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
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
        let max = limits.max();
        let loose = layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, max.height));
        let mut nodes: Vec<layout::Node> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .map(|(child, tree)| child.as_widget_mut().layout(tree, renderer, &loose))
            .collect();

        let width = |shown: &[usize], nodes: &[layout::Node]| -> f32 {
            let widths: f32 = shown.iter().map(|&c| nodes[c].size().width).sum();
            widths + self.spacing * shown.len().saturating_sub(1) as f32
        };
        let hidden = (0..self.n)
            .find(|&hidden| width(&self.shown(hidden), &nodes) <= max.width)
            .unwrap_or(self.n - 1);
        let shown = self.shown(hidden);

        // The current folder alone is too wide: lay it out again in what the
        // rest leaves
        let current = self.crumb(self.n - 1);
        let rest = width(&shown, &nodes) - nodes[current].size().width;
        if rest + nodes[current].size().width > max.width {
            let room = (max.width - rest).max(0.0);
            nodes[current] = self.children[current].as_widget_mut().layout(
                &mut tree.children[current],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(room, max.height)),
            );
        }

        let height = shown
            .iter()
            .map(|&c| nodes[c].size().height)
            .fold(0.0, f32::max);
        let mut x = 0.0;
        for &c in &shown {
            let size = nodes[c].size();
            let node = std::mem::take(&mut nodes[c]);
            nodes[c] = node.move_to((x, 0.0)).align(
                Alignment::Start,
                Alignment::Center,
                Size::new(size.width, height),
            );
            x += size.width + self.spacing;
        }
        for (c, node) in nodes.iter_mut().enumerate() {
            if !shown.contains(&c) {
                *node = layout::Node::new(Size::ZERO);
            }
        }
        tree.state.downcast_mut::<State>().hidden = hidden;

        let width = (x - self.spacing).max(0.0);
        layout::Node::with_children(
            limits.resolve(Length::Shrink, Length::Shrink, Size::new(width, height)),
            nodes,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let hidden = tree.state.downcast_ref::<State>().hidden;
        let shown = self.live(hidden);
        for (c, ((child, tree), layout)) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .enumerate()
        {
            if shown.contains(&c) {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
        }
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
        let hidden = tree.state.downcast_ref::<State>().hidden;
        let shown = self.live(hidden);
        for (c, ((child, tree), layout)) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .enumerate()
        {
            if shown.contains(&c) {
                child.as_widget_mut().update(
                    tree, event, layout, cursor, renderer, clipboard, shell, viewport,
                );
            }
        }

        // A right press on a crumb opens its context menu, which leaves the
        // press uncaptured; it is taken here so that what holds the crumbs
        // (the header, whose right press opens the window menu) does not act
        // on it too
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event
            && layout
                .children()
                .enumerate()
                .any(|(c, child)| shown.contains(&c) && cursor.is_over(child.bounds()))
        {
            shell.capture_event();
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
        let hidden = tree.state.downcast_ref::<State>().hidden;
        let shown = self.live(hidden);
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .enumerate()
            .filter(|(c, _)| shown.contains(c))
            .map(|(_, ((child, tree), layout))| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        let hidden = tree.state.downcast_ref::<State>().hidden;
        let shown = self.live(hidden);
        for (c, ((child, tree), layout)) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .enumerate()
        {
            if shown.contains(&c) {
                child
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
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
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let hidden = tree.state.downcast_ref::<State>().hidden;
        let shown = self.live(hidden);
        let overlays: Vec<_> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .enumerate()
            .filter(|(c, _)| shown.contains(c))
            .filter_map(|(_, ((child, tree), layout))| {
                child
                    .as_widget_mut()
                    .overlay(tree, layout, renderer, viewport, translation)
            })
            .collect();
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
    }
}

impl<'a, Message: 'a> From<Crumbs<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(crumbs: Crumbs<'a, Message>) -> Self {
        Element::new(crumbs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::widget::Id;
    use iced_core::{Point, clipboard};
    use iced_runtime::user_interface::{Cache, UserInterface};

    use crate::mouse_area::MouseArea;
    use crate::ui::widget::{container, id_container, space};

    /// A child `width` wide and `height` high, reporting its bounds as `id`
    /// and sending `id` when pressed. Its box shrinks to the width it is
    /// given.
    fn sized(
        id: &'static str,
        width: f32,
        height: f32,
    ) -> Element<'static, &'static str, Theme, Renderer> {
        id_container(
            MouseArea::new(container(
                space::horizontal()
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(height)),
            ))
            .on_press(move |_| id),
            Id::new(id),
        )
        .into()
    }

    /// A child `width` wide and 20 high, reporting its bounds as `id` and
    /// sending `id` when pressed. Its box shrinks to the width it is given.
    fn child(id: &'static str, width: f32) -> Element<'static, &'static str, Theme, Renderer> {
        sized(id, width, 20.0)
    }

    /// Crumbs `a`, `b` and `c`, 100 wide each, under a 20 wide icon, with 10
    /// wide separators and 20 wide "more" elements: 340 wide in full.
    fn view() -> Element<'static, &'static str, Theme, Renderer> {
        crumbs(
            child("icon", 20.0),
            vec![child("a", 100.0), child("b", 100.0), child("c", 100.0)],
            vec![child("sep0", 10.0), child("sep1", 10.0)],
            vec![child("more0", 20.0), child("more1", 20.0)],
        )
        .into()
    }

    fn build(
        view: Element<'static, &'static str, Theme, Renderer>,
        width: f32,
    ) -> (
        UserInterface<'static, &'static str, Theme, Renderer>,
        Renderer,
    ) {
        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let ui = UserInterface::build(
            // Starts at the left, so each child's x is its place in the row
            crate::ui::widget::Row::new().push(container(view).max_width(width)),
            Size::new(1000.0, 100.0),
            Cache::default(),
            &mut renderer,
        );
        (ui, renderer)
    }

    /// Each shown child's id and bounds, left to right.
    fn shown(
        view: Element<'static, &'static str, Theme, Renderer>,
        width: f32,
    ) -> Vec<(Id, Rectangle)> {
        struct Shown(Vec<(Id, Rectangle)>);
        impl Operation for Shown {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
                if let Some(id) = id {
                    self.0.push((id.clone(), bounds));
                }
            }
        }
        let (mut ui, renderer) = build(view, width);
        let mut shown = Shown(Vec::new());
        ui.operate(&renderer, &mut shown);
        shown.0.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
        shown.0
    }

    fn ids(shown: &[(Id, Rectangle)]) -> Vec<Id> {
        shown.iter().map(|(id, _)| id.clone()).collect()
    }

    #[test]
    fn everything_shows_when_it_fits() {
        let shown = shown(view(), 400.0);
        assert_eq!(
            ids(&shown),
            ["icon", "a", "sep0", "b", "sep1", "c"].map(Id::new)
        );
        let (_, c) = &shown[5];
        assert_eq!(c.x + c.width, 340.0);
    }

    #[test]
    fn leading_crumbs_give_way_to_the_last_hidden_ones_more() {
        assert_eq!(
            ids(&shown(view(), 300.0)),
            ["icon", "more0", "sep0", "b", "sep1", "c"].map(Id::new)
        );
        assert_eq!(
            ids(&shown(view(), 200.0)),
            ["icon", "more1", "sep1", "c"].map(Id::new)
        );
    }

    #[test]
    fn the_current_folder_takes_what_is_left_when_it_alone_does_not_fit() {
        let shown = shown(view(), 100.0);
        assert_eq!(ids(&shown), ["icon", "more1", "sep1", "c"].map(Id::new));
        let (_, c) = &shown[3];
        assert_eq!((c.x, c.width), (50.0, 50.0));
    }

    #[test]
    fn a_press_where_a_hidden_crumb_would_be_reaches_what_is_shown_there() {
        // Hiding `a`, the "more" standing for it sits where `a` started
        let (mut ui, mut renderer) = build(view(), 300.0);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(30.0, 10.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, ["more0"]);
    }

    #[test]
    fn the_spacing_counts_and_shorter_children_are_centred() {
        // As `view`, with 5 between neighbours and a 10 high icon: 365 wide
        // in full, the 20 high crumbs setting the row's height
        let spaced = || {
            crumbs(
                sized("icon", 20.0, 10.0),
                vec![child("a", 100.0), child("b", 100.0), child("c", 100.0)],
                vec![child("sep0", 10.0), child("sep1", 10.0)],
                vec![child("more0", 20.0), child("more1", 20.0)],
            )
            .spacing(5.0)
            .into()
        };
        let full = shown(spaced(), 365.0);
        assert_eq!(
            ids(&full),
            ["icon", "a", "sep0", "b", "sep1", "c"].map(Id::new)
        );
        let (_, icon) = &full[0];
        assert_eq!(icon.y, 5.0);
        let (_, c) = &full[5];
        assert_eq!(c.x + c.width, 365.0);
        assert_eq!(
            ids(&shown(spaced(), 364.0)),
            ["icon", "more0", "sep0", "b", "sep1", "c"].map(Id::new)
        );
    }

    #[test]
    fn a_right_press_on_a_crumb_goes_no_further() {
        // What holds the crumbs acts on a right press, as the header opens
        // the window menu on one
        let held = crate::mouse_area::MouseArea::new(view()).on_right_press(|_| "window");
        let (mut ui, mut renderer) = build(held.into(), 400.0);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            // Over crumb `a`, at 20 to 120
            mouse::Cursor::Available(Point::new(70.0, 10.0)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    #[test]
    fn an_invisible_row_only_takes_up_its_room() {
        // What follows starts where the whole path ends, but nothing of the
        // path is reached by an operation
        let row = crate::ui::widget::Row::new()
            .push(Element::from(
                crumbs(
                    child("icon", 20.0),
                    vec![child("a", 100.0), child("b", 100.0), child("c", 100.0)],
                    vec![child("sep0", 10.0), child("sep1", 10.0)],
                    vec![child("more0", 20.0), child("more1", 20.0)],
                )
                .invisible(),
            ))
            .push(child("after", 10.0));
        let shown = shown(row.into(), 400.0);
        assert_eq!(ids(&shown), [Id::new("after")]);
        assert_eq!(shown[0].1.x, 340.0);
    }

    #[test]
    fn a_single_crumb_takes_what_the_icon_leaves() {
        let single = crumbs(child("icon", 20.0), vec![child("c", 100.0)], vec![], vec![]);
        let shown = shown(single.into(), 50.0);
        assert_eq!(ids(&shown), ["icon", "c"].map(Id::new));
        let (_, c) = &shown[1];
        assert_eq!((c.x, c.width), (20.0, 30.0));
    }
}
