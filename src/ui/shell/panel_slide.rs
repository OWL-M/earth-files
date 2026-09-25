// SPDX-License-Identifier: GPL-3.0-only

//! A side panel's slide in from its window edge and back out: the context
//! drawer on the right, the nav bar on the left.
//!
//! The shell observes whether each panel is shown rather than hooking a
//! setter, because several sites write those states directly. A change from
//! rest starts a slide on fresh tracks, from the edge at the panel's current
//! width; a change mid-slide turns the running slide round, keeping its
//! velocity.
//!
//! Two paths. On [`SlidePath::Columns`] (the drawer only) the layout stays
//! wide while the drawer floats in over it and the list's fixed columns
//! slide left as textures; the one relayout happens when the slide comes to
//! rest. On [`SlidePath::Snap`] the layout goes straight to where it is going
//! and only the panel moves. Neither lays anything out per frame.

use iced_core::Vector;
use iced_texture_cache::TextureCache;
use iced_texture_cache::iced_animate::{Anim, Curve, Motion, MotionKey, curves};

use crate::ui::Element;
use crate::ui::iced::Length;

/// A spring, so turning round mid-slide keeps velocity; critically damped,
/// so the panel never overshoots its edge.
const CURVE: Curve = curves::QUICK;

/// The window edge a panel slides in from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Left,
    Right,
}

impl Edge {
    /// Which way is off-screen.
    const fn outward(self) -> f32 {
        match self {
            Self::Left => -1.0,
            Self::Right => 1.0,
        }
    }
}

/// How the file view follows a slide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlidePath {
    /// The list's fixed columns slide as textures over a wide layout.
    Columns,
    /// The layout snaps to its destination; only the panel moves.
    Snap,
}

/// What the tab needs while a column slide runs.
#[derive(Clone, Debug)]
pub struct ColumnSlide {
    /// How much width the drawer takes from the file view.
    pub extent: f32,
    /// The columns' offset: `0 → −extent` opening, `−extent → 0` closing.
    pub offset: Anim<Vector>,
}

/// A panel's slide state. Cloneable because `Core` is; clones share the
/// engine and tracks but not the observation state, so only the one in
/// `Core` is ever synced.
#[derive(Clone)]
pub(crate) struct PanelSlide {
    motion: Motion,
    edge: Edge,
    panel_key: MotionKey,
    columns_key: MotionKey,
    /// The panel's offset from its resting place: `outward · extent` is
    /// off-screen.
    panel: Anim<Vector>,
    /// See [`ColumnSlide::offset`].
    columns: Anim<Vector>,
    /// Whether the panel was shown, as last observed; `None` until the first
    /// observation.
    shown: Option<bool>,
    /// `is_condensed` as last observed.
    condensed: bool,
    /// The extent the running (or last) slide started with.
    extent: f32,
    path: SlidePath,
}

impl PanelSlide {
    /// The context drawer's slide, on `motion`.
    pub(crate) fn drawer(motion: Motion) -> Self {
        Self::new(motion, Edge::Right)
    }

    /// The nav bar's slide, on `motion`.
    pub(crate) fn nav(motion: Motion) -> Self {
        Self::new(motion, Edge::Left)
    }

    fn new(motion: Motion, edge: Edge) -> Self {
        Self {
            motion,
            edge,
            panel_key: MotionKey::unique(),
            columns_key: MotionKey::unique(),
            panel: Anim::constant(Vector::ZERO),
            columns: Anim::constant(Vector::ZERO),
            shown: None,
            condensed: false,
            extent: 0.0,
            path: SlidePath::Snap,
        }
    }

    /// The engine the slide runs on; the view wraps itself in its host.
    pub(crate) const fn motion(&self) -> &Motion {
        &self.motion
    }

    /// Whether the panel is in motion.
    pub(crate) fn is_moving(&self) -> bool {
        self.panel.is_animating()
    }

    /// The path the running (or last) slide took.
    pub(crate) const fn path(&self) -> SlidePath {
        self.path
    }

    /// Whether observing `shown` would start a slide from rest, which is
    /// when [`sync`](Self::sync) needs a real extent and path check.
    pub(crate) fn starts_slide(&self, shown: bool) -> bool {
        self.shown.is_some_and(|was| was != shown) && !self.is_moving()
    }

    /// Observes whether the panel is shown, and `is_condensed`, after an
    /// update.
    ///
    /// `extent` and `columns_fit` are only read when a slide starts from
    /// rest (see [`starts_slide`](Self::starts_slide)); pass anything
    /// otherwise. `condensed` is `Core::is_condensed` as `view_main` will
    /// read it. `columns_fit` is ignored on the left edge.
    pub(crate) fn sync(&mut self, shown: bool, condensed: bool, extent: f32, columns_fit: bool) {
        let from_rest = self.starts_slide(shown);
        let was_condensed = std::mem::replace(&mut self.condensed, condensed);
        let Some(was) = self.shown.replace(shown) else {
            // The first look is the starting state, not a change.
            return;
        };

        // The nav is shown by one toggle when wide and another when
        // condensed, so crossing the breakpoint can show or hide it without
        // anyone asking. That is the layout's doing, not a toggle: snap.
        // Crossing it also changes the panel's width, which a slide already
        // running would go on sliding at, so that stops too, shown or not.
        if self.edge == Edge::Left && was_condensed != condensed {
            self.panel = Anim::constant(Vector::ZERO);
            self.columns = Anim::constant(Vector::ZERO);
            return;
        }
        if was == shown {
            return;
        }

        let outward = self.edge.outward();
        if !from_rest {
            // Turned round mid-slide: carry on from where it is, with its
            // velocity, on the path and at the extent it started with.
            let extent = self.extent;
            let (panel, columns) = if shown {
                (0.0, -extent)
            } else {
                (outward * extent, 0.0)
            };
            self.panel = self
                .motion
                .to(self.panel_key, CURVE, Vector::new(panel, 0.0));
            if self.edge == Edge::Right {
                self.columns = self
                    .motion
                    .to(self.columns_key, CURVE, Vector::new(columns, 0.0));
            }
            return;
        }

        // `enter` asserts on non-finite values; a bad width is no slide.
        self.extent = if extent.is_finite() {
            extent.max(0.0)
        } else {
            0.0
        };
        let extent = self.extent;
        self.path = if self.edge == Edge::Right && columns_fit && !condensed && !was_condensed {
            SlidePath::Columns
        } else {
            SlidePath::Snap
        };
        // Fresh keys, so `enter` starts from the edge at today's extent
        // rather than wherever an old track came to rest.
        self.panel_key = MotionKey::unique();
        self.columns_key = MotionKey::unique();
        let (from, to) = if shown {
            (outward * extent, 0.0)
        } else {
            (0.0, outward * extent)
        };
        self.panel = self.motion.enter(
            self.panel_key,
            CURVE,
            Vector::new(from, 0.0),
            Vector::new(to, 0.0),
        );
        if self.edge == Edge::Right {
            self.columns = self.motion.enter(
                self.columns_key,
                CURVE,
                Vector::new(from - extent, 0.0),
                Vector::new(to - extent, 0.0),
            );
        }
    }

    /// What the tab needs, while a column slide runs.
    pub(crate) fn column_slide(&self) -> Option<ColumnSlide> {
        (self.is_moving() && self.path == SlidePath::Columns).then(|| ColumnSlide {
            extent: self.extent,
            offset: self.columns.clone(),
        })
    }

    /// Wraps `content` so it publishes `on_settled` once each slide comes to
    /// rest. `view_main` keeps one around a layer of the root tree, under
    /// the host, so it sees each frame's value after the tick; inside an
    /// overlay it would see it before.
    pub(crate) fn watch<'a, Message: Clone + 'a>(
        &self,
        on_settled: Message,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        crate::ui::widget::settle_watch(self.panel.clone(), self.panel_key, on_settled, content)
            .into()
    }

    /// The panel, moved by the slide. It stays wrapped at rest as well, drawn
    /// live then, so starting or ending a slide never changes the panel's
    /// place in the tree — which would cost it its scroll offsets and focus.
    /// While it moves it is one texture, recorded from a fresh cache on every
    /// view build, so it always shows what the panel shows now.
    pub(crate) fn translated<'a, Message: 'a>(
        &self,
        panel: Element<'a, Message>,
    ) -> Element<'a, Message> {
        iced_texture_cache::cached(TextureCache::new(), panel)
            .translate(self.panel.clone())
            .live_at_rest(true)
            .into()
    }

    /// Places a built panel: returns the slot for the content row and the
    /// panel for the stack layer above it.
    ///
    /// The panel itself always goes in the layer: one place in the tree
    /// however it is laid out. `inline` only decides whether the row keeps
    /// `slot` free for it — the width it covers at the row's edge — or lays
    /// the content out underneath it.
    pub(crate) fn place<'a, Message: 'a>(
        &self,
        panel: Option<Element<'a, Message>>,
        inline: bool,
        slot: Length,
    ) -> (Element<'a, Message>, Element<'a, Message>) {
        use crate::ui::widget::space;

        let slot = if inline && panel.is_some() {
            slot
        } else {
            Length::Shrink
        };
        let panel = panel.map_or_else(
            || space::horizontal().width(Length::Shrink).into(),
            |panel| self.translated(panel),
        );
        (space::horizontal().width(slot).into(), panel)
    }

    /// The key of the running (or last) slide's panel track.
    #[cfg(test)]
    const fn key(&self) -> MotionKey {
        self.panel_key
    }
}

/// The layers above the content row that hold the panels: the nav's aligned
/// left, the drawer's aligned right.
///
/// One layer each, so neither can take the other's width: condensed, a nav
/// covers the window, and while it slides out a drawer may be sliding in
/// beside it. `nav_fills` is whether the nav is window-wide, where its layer
/// must not also give room to a gap after it.
pub(crate) fn layers<'a, Message: 'a>(
    nav: Element<'a, Message>,
    nav_fills: bool,
    drawer: Element<'a, Message>,
) -> (Element<'a, Message>, Element<'a, Message>) {
    use crate::ui::widget::{Row, space};

    let gap = if nav_fills {
        Length::Shrink
    } else {
        Length::Fill
    };
    let nav = Row::with_children(vec![nav, space::horizontal().width(gap).into()])
        .height(Length::Fill)
        .into();
    let drawer = Row::with_children(vec![space::horizontal().width(Length::Fill).into(), drawer])
        .height(Length::Fill)
        .into();
    (nav, drawer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_texture_cache::iced_animate::testing::FrameClock;

    const EPS: f32 = 1e-3;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn the_first_look_latches_without_sliding() {
        let mut slide = PanelSlide::drawer(Motion::new());
        assert!(!slide.starts_slide(true), "nothing observed yet");

        slide.sync(true, false, 400.0, true);

        assert!(!slide.is_moving());
        assert!(slide.column_slide().is_none());
    }

    #[test]
    fn opening_slides_in_from_the_edge_and_settles() {
        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);

        assert!(slide.starts_slide(true));
        slide.sync(true, false, 400.0, true);

        assert!(slide.is_moving());
        assert_eq!(slide.path(), SlidePath::Columns);
        assert!(close(slide.panel.get().x, 400.0), "starts off-screen");
        assert!(close(slide.panel.target().x, 0.0));
        let columns = slide.column_slide().expect("a column slide is running");
        assert!(close(columns.extent, 400.0));
        assert!(close(columns.offset.get().x, 0.0));
        assert!(close(columns.offset.target().x, -400.0));

        let _ = clock.run_until_settled();

        assert!(!slide.is_moving());
        assert!(slide.column_slide().is_none());
    }

    #[test]
    fn closing_slides_out_and_brings_the_columns_back() {
        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(true, false, 0.0, false);

        slide.sync(false, false, 400.0, true);

        assert!(slide.is_moving());
        assert!(close(slide.panel.get().x, 0.0));
        assert!(close(slide.panel.target().x, 400.0));
        let columns = slide.column_slide().expect("a column slide is running");
        assert!(close(columns.offset.get().x, -400.0));
        assert!(close(columns.offset.target().x, 0.0));

        let _ = clock.run_until_settled();
        assert!(!slide.is_moving());
    }

    #[test]
    fn turning_round_mid_slide_keeps_the_track_path_and_extent() {
        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 400.0, true);
        let _ = clock.run(3);
        let key = slide.key();

        assert!(!slide.starts_slide(false), "a reversal is not a new slide");
        // A different extent and no fit: both must be ignored mid-slide.
        slide.sync(false, false, 999.0, false);

        assert!(slide.is_moving());
        assert_eq!(slide.key(), key);
        assert_eq!(slide.path(), SlidePath::Columns);
        assert!(close(slide.panel.target().x, 400.0));
        assert!(close(slide.columns.target().x, 0.0));
        // It carries on from part-way, not from the edge.
        let x = slide.panel.get().x;
        assert!(x > 0.0 && x < 400.0, "restarted at {x}");
        let c = slide.columns.get().x;
        assert!(c < 0.0 && c > -400.0, "columns restarted at {c}");
    }

    #[test]
    fn the_columns_path_needs_a_fit_and_no_condensing() {
        let mut no_fit = PanelSlide::drawer(Motion::new());
        no_fit.sync(false, false, 0.0, false);
        no_fit.sync(true, false, 400.0, false);
        assert_eq!(no_fit.path(), SlidePath::Snap);
        assert!(no_fit.column_slide().is_none());

        let mut condenses = PanelSlide::drawer(Motion::new());
        condenses.sync(false, false, 0.0, false);
        condenses.sync(true, true, 400.0, true);
        assert_eq!(condenses.path(), SlidePath::Snap);

        let mut was_condensed = PanelSlide::drawer(Motion::new());
        was_condensed.sync(true, true, 0.0, false);
        was_condensed.sync(false, false, 400.0, true);
        assert_eq!(was_condensed.path(), SlidePath::Snap);
    }

    #[test]
    fn an_unchanged_observation_does_nothing() {
        let mut slide = PanelSlide::drawer(Motion::new());
        slide.sync(true, false, 0.0, false);
        assert!(!slide.starts_slide(true));
        slide.sync(true, false, 400.0, true);
        assert!(!slide.is_moving());
    }

    #[test]
    fn turning_round_twice_mid_slide_stays_on_one_track() {
        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 400.0, true);
        let _ = clock.run(3);
        let key = slide.key();

        slide.sync(false, false, 0.0, false);
        let _ = clock.run(3);
        slide.sync(true, false, 0.0, false);

        assert!(slide.is_moving());
        assert_eq!(slide.key(), key, "still the same track");
        assert_eq!(slide.path(), SlidePath::Columns);
        assert!(close(slide.panel.target().x, 0.0));
        assert!(close(slide.columns.target().x, -400.0));
    }

    #[test]
    fn a_slide_after_rest_starts_afresh_at_the_new_width() {
        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 400.0, true);
        let _ = clock.run_until_settled();
        let key = slide.key();

        slide.sync(false, false, 300.0, false);

        assert_ne!(slide.key(), key, "a fresh slide from rest, a fresh track");
        assert_eq!(slide.path(), SlidePath::Snap);
        assert!(close(slide.panel.get().x, 0.0), "starts at rest");

        let _ = clock.run_until_settled();
        assert!(close(slide.panel.get().x, 300.0));
    }

    /// Scrolls every scrollable it meets to `to`, if set, and records the
    /// vertical offset each one had.
    struct Scroll {
        to: Option<f32>,
        seen: Vec<f32>,
    }

    impl iced_core::widget::Operation for Scroll {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced_core::widget::Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&iced_core::widget::Id>,
            _bounds: iced_core::Rectangle,
            _content_bounds: iced_core::Rectangle,
            translation: Vector,
            state: &mut dyn iced_core::widget::operation::Scrollable,
        ) {
            self.seen.push(translation.y);
            if let Some(y) = self.to {
                state.scroll_to(iced_core::widget::operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                });
            }
        }
    }

    /// A window holding a content row and, above it, a scrollable drawer
    /// placed by `slide` as `view_main` places it.
    fn window(slide: &PanelSlide, shown: bool) -> Element<'static, ()> {
        use crate::ui::widget::{Row, Stack, scrollable, space};

        let inline = shown && !(slide.is_moving() && slide.path() == SlidePath::Columns);
        let drawer = scrollable(space::vertical().height(Length::Fixed(400.0)))
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(100.0));
        let (slot, panel) = slide.place(Some(drawer.into()), inline, Length::Fixed(100.0));
        Stack::with_children(vec![
            Row::with_children(vec![space::horizontal().width(Length::Fill).into(), slot]).into(),
            Row::with_children(vec![space::horizontal().width(Length::Fill).into(), panel])
                .height(Length::Fill)
                .into(),
        ])
        .into()
    }

    #[test]
    fn the_drawer_keeps_its_scroll_through_a_slide_and_its_end() {
        use iced_runtime::user_interface::{Cache, UserInterface};

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let size = iced_core::Size::new(300.0, 100.0);
        let offset = |ui: &mut UserInterface<'_, (), _, _>, renderer: &_| {
            let mut read = Scroll {
                to: None,
                seen: Vec::new(),
            };
            ui.operate(renderer, &mut read);
            read.seen
        };

        let mut slide = PanelSlide::drawer(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 100.0, false);
        assert!(slide.is_moving(), "opening");

        let mut ui =
            UserInterface::build(window(&slide, true), size, Cache::default(), &mut renderer);
        ui.operate(
            &renderer,
            &mut Scroll {
                to: Some(50.0),
                seen: Vec::new(),
            },
        );
        assert_eq!(offset(&mut ui, &renderer), vec![50.0], "scrolled mid-slide");

        let _ = clock.run_until_settled();
        assert!(!slide.is_moving(), "open, at rest");
        let mut ui =
            UserInterface::build(window(&slide, true), size, ui.into_cache(), &mut renderer);
        assert_eq!(
            offset(&mut ui, &renderer),
            vec![50.0],
            "after the opening settles"
        );

        slide.sync(false, false, 100.0, false);
        assert!(slide.is_moving(), "closing");
        let mut ui =
            UserInterface::build(window(&slide, false), size, ui.into_cache(), &mut renderer);
        assert_eq!(
            offset(&mut ui, &renderer),
            vec![50.0],
            "as the close starts"
        );
    }

    #[test]
    fn the_nav_slides_in_from_the_left_edge() {
        let mut slide = PanelSlide::nav(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);

        slide.sync(true, false, 300.0, true);

        assert!(slide.is_moving());
        assert_eq!(slide.path(), SlidePath::Snap, "no column path on the left");
        assert!(slide.column_slide().is_none());
        assert!(close(slide.panel.get().x, -300.0), "starts off-screen");
        assert!(close(slide.panel.target().x, 0.0));

        let _ = clock.run_until_settled();
        assert!(!slide.is_moving());
    }

    #[test]
    fn the_nav_slides_out_to_the_left_and_turns_round() {
        let mut slide = PanelSlide::nav(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(true, false, 0.0, false);

        slide.sync(false, false, 300.0, false);
        assert!(close(slide.panel.target().x, -300.0));
        let _ = clock.run(3);
        let x = slide.panel.get().x;
        assert!(x < 0.0 && x > -300.0, "part-way out at {x}");

        slide.sync(true, false, 999.0, false);
        assert!(slide.is_moving());
        assert!(
            close(slide.panel.target().x, 0.0),
            "back in, at the old extent"
        );
    }

    #[test]
    fn the_nav_snaps_when_the_window_crosses_the_breakpoint() {
        let mut slide = PanelSlide::nav(Motion::new());
        slide.sync(true, false, 0.0, false);
        slide.sync(false, true, 300.0, false);
        assert!(!slide.is_moving(), "hidden by condensing, not by a toggle");

        // Mid-slide as well.
        let mut slide = PanelSlide::nav(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 300.0, false);
        let _ = clock.run(3);
        slide.sync(false, true, 0.0, false);
        assert!(!slide.is_moving());
    }

    #[test]
    fn crossing_the_breakpoint_mid_slide_stops_the_slide_at_its_old_width() {
        let mut slide = PanelSlide::nav(Motion::new());
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(true, false, 0.0, false);
        slide.sync(false, false, 300.0, false);
        let _ = clock.run(3);
        assert!(slide.is_moving(), "closing, wide");

        // Still hidden, but now condensed: 300px is no longer the panel's width.
        slide.sync(false, true, 0.0, false);
        assert!(!slide.is_moving());
    }

    /// Records the bounds of each `id_container` it meets, by id.
    struct Bounds(Vec<(iced_core::widget::Id, iced_core::Rectangle)>);

    impl iced_core::widget::Operation for Bounds {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced_core::widget::Operation)) {
            operate(self);
        }

        fn container(&mut self, id: Option<&iced_core::widget::Id>, bounds: iced_core::Rectangle) {
            if let Some(id) = id {
                self.0.push((id.clone(), bounds));
            }
        }
    }

    /// Where the nav and the drawer land when laid out by [`layers`] in a
    /// window `width` wide.
    fn laid_out(
        width: f32,
        nav: Length,
        nav_fills: bool,
    ) -> (iced_core::Rectangle, iced_core::Rectangle) {
        use crate::ui::widget::{Stack, id_container, space};
        use iced_runtime::user_interface::{Cache, UserInterface};

        let nav_id = iced_core::widget::Id::new("nav");
        let drawer_id = iced_core::widget::Id::new("drawer");
        let nav = id_container(
            space::horizontal().width(nav).height(Length::Fill),
            nav_id.clone(),
        );
        let drawer = id_container(
            space::horizontal()
                .width(Length::Fixed(100.0))
                .height(Length::Fill),
            drawer_id.clone(),
        );
        let (nav_layer, drawer_layer) = layers(nav.into(), nav_fills, drawer.into());
        let window: Element<'_, ()> = Stack::with_children(vec![
            space::horizontal().width(Length::Fill).into(),
            nav_layer,
            drawer_layer,
        ])
        .into();

        let mut renderer = iced_texture_cache::testing::headless_tiny_skia();
        let ui = UserInterface::build(
            window,
            iced_core::Size::new(width, 100.0),
            Cache::default(),
            &mut renderer,
        );
        let mut ui = ui;
        let mut bounds = Bounds(Vec::new());
        ui.operate(&renderer, &mut bounds);
        let find = |id: &iced_core::widget::Id| {
            bounds
                .0
                .iter()
                .find(|(seen, _)| seen == id)
                .map(|(_, bounds)| *bounds)
                .expect("laid out")
        };
        (find(&nav_id), find(&drawer_id))
    }

    #[test]
    fn a_window_wide_nav_leaves_the_drawer_its_width() {
        // Condensed: the nav sliding out still covers the window while the
        // drawer slides in.
        let (nav, drawer) = laid_out(500.0, Length::Fill, true);
        assert!(close(nav.width, 500.0), "nav {nav:?}");
        assert!(close(drawer.x, 400.0), "drawer {drawer:?}");
        assert!(close(drawer.width, 100.0), "drawer {drawer:?}");
    }

    #[test]
    fn wide_the_panels_keep_to_their_edges() {
        let (nav, drawer) = laid_out(500.0, Length::Fixed(200.0), false);
        assert!(close(nav.x, 0.0) && close(nav.width, 200.0), "nav {nav:?}");
        assert!(
            close(drawer.x, 400.0) && close(drawer.width, 100.0),
            "drawer {drawer:?}"
        );
    }

    #[test]
    fn the_drawer_still_slides_across_the_breakpoint() {
        let mut slide = PanelSlide::drawer(Motion::new());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, true, 400.0, true);
        assert!(slide.is_moving());
        assert_eq!(slide.path(), SlidePath::Snap);
    }
}
