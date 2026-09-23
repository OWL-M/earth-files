// SPDX-License-Identifier: GPL-3.0-only

//! The context drawer's slide in from the right edge and back out.
//!
//! The shell observes `show_context` rather than hooking the one setter,
//! because several sites write it directly. A change from rest starts a
//! slide on fresh tracks, from the edge at the drawer's current width; a
//! change mid-slide turns the running slide round, keeping its velocity.
//!
//! Two paths. On [`SlidePath::Columns`] the layout stays wide while the
//! drawer floats in over it and the list's fixed columns slide left as
//! textures; the one relayout happens when the slide comes to rest. On
//! [`SlidePath::Snap`] the layout goes straight to where it is going and
//! only the drawer moves. Neither lays anything out per frame.

use iced_core::Vector;
use iced_texture_cache::TextureCache;
use iced_texture_cache::iced_animate::{Anim, Curve, Motion, MotionKey, curves};

use crate::ui::Element;

/// A spring, so turning round mid-slide keeps velocity; critically damped,
/// so the drawer never overshoots its edge.
const CURVE: Curve = curves::QUICK;

/// How the file view follows a slide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlidePath {
    /// The list's fixed columns slide as textures over a wide layout.
    Columns,
    /// The layout snaps to its destination; only the drawer moves.
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

/// The drawer's slide state. Cloneable because `Core` is; clones share
/// the engine and tracks but not the observation state, so only the one
/// in `Core` is ever synced.
#[derive(Clone)]
pub(crate) struct DrawerSlide {
    motion: Motion,
    drawer_key: MotionKey,
    columns_key: MotionKey,
    /// The drawer's offset from its resting place: `+extent` is off-screen.
    drawer: Anim<Vector>,
    /// See [`ColumnSlide::offset`].
    columns: Anim<Vector>,
    /// `show_context` as last observed; `None` until the first observation.
    shown: Option<bool>,
    /// `is_condensed` as last observed.
    condensed: bool,
    /// The extent the running (or last) slide started with.
    extent: f32,
    path: SlidePath,
}

impl DrawerSlide {
    pub(crate) fn new() -> Self {
        Self {
            motion: Motion::new(),
            drawer_key: MotionKey::unique(),
            columns_key: MotionKey::unique(),
            drawer: Anim::constant(Vector::ZERO),
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

    /// Whether the drawer is in motion.
    pub(crate) fn is_moving(&self) -> bool {
        self.drawer.is_animating()
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

    /// Observes `show_context` and `is_condensed` after an update.
    ///
    /// `extent` and `columns_fit` are only read when a slide starts from
    /// rest (see [`starts_slide`](Self::starts_slide)); pass anything
    /// otherwise. `condensed` is `Core::is_condensed` as `view_main` will
    /// read it.
    pub(crate) fn sync(&mut self, shown: bool, condensed: bool, extent: f32, columns_fit: bool) {
        let from_rest = self.starts_slide(shown);
        let was_condensed = std::mem::replace(&mut self.condensed, condensed);
        let Some(was) = self.shown.replace(shown) else {
            // The first look is the starting state, not a change.
            return;
        };
        if was == shown {
            return;
        }

        if !from_rest {
            // Turned round mid-slide: carry on from where it is, with its
            // velocity, on the path and at the extent it started with.
            let extent = self.extent;
            let (drawer, columns) = if shown { (0.0, -extent) } else { (extent, 0.0) };
            self.drawer = self
                .motion
                .to(self.drawer_key, CURVE, Vector::new(drawer, 0.0));
            self.columns = self
                .motion
                .to(self.columns_key, CURVE, Vector::new(columns, 0.0));
            return;
        }

        // `enter` asserts on non-finite values; a bad width is no slide.
        self.extent = if extent.is_finite() {
            extent.max(0.0)
        } else {
            0.0
        };
        let extent = self.extent;
        self.path = if columns_fit && !condensed && !was_condensed {
            SlidePath::Columns
        } else {
            SlidePath::Snap
        };
        // Fresh keys, so `enter` starts from the edge at today's extent
        // rather than wherever an old track came to rest.
        self.drawer_key = MotionKey::unique();
        self.columns_key = MotionKey::unique();
        let (from, to) = if shown { (extent, 0.0) } else { (0.0, extent) };
        self.drawer = self.motion.enter(
            self.drawer_key,
            CURVE,
            Vector::new(from, 0.0),
            Vector::new(to, 0.0),
        );
        self.columns = self.motion.enter(
            self.columns_key,
            CURVE,
            Vector::new(from - extent, 0.0),
            Vector::new(to - extent, 0.0),
        );
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
        crate::ui::widget::settle_watch(self.drawer.clone(), self.drawer_key, on_settled, content)
            .into()
    }

    /// The drawer, moved by the slide. It stays wrapped at rest as well, drawn
    /// live then, so starting or ending a slide never changes the drawer's
    /// place in the tree — which would cost it its scroll offsets and focus.
    /// While it moves it is one texture, recorded from a fresh cache on every
    /// view build, so it always shows what the drawer shows now.
    pub(crate) fn translated<'a, Message: 'a>(
        &self,
        drawer: Element<'a, Message>,
    ) -> Element<'a, Message> {
        iced_texture_cache::cached(TextureCache::new(), drawer)
            .translate(self.drawer.clone())
            .live_at_rest(true)
            .into()
    }

    /// Places a built drawer: returns the slot for the content row and the
    /// content of the stack layer above it.
    ///
    /// The drawer itself is always in the layer, right-aligned: one place in
    /// the tree however it is laid out. `inline` only decides whether the row
    /// keeps `slot_width` free for it — the width the drawer covers at the
    /// row's end — or lays the content out underneath it.
    pub(crate) fn place<'a, Message: 'a>(
        &self,
        drawer: Option<Element<'a, Message>>,
        inline: bool,
        slot_width: f32,
    ) -> (Element<'a, Message>, Element<'a, Message>) {
        use crate::ui::iced::Length;
        use crate::ui::widget::{Row, space};

        let slot = if inline && drawer.is_some() {
            Length::Fixed(slot_width)
        } else {
            Length::Shrink
        };
        let drawer = drawer.map_or_else(
            || space::horizontal().width(Length::Shrink).into(),
            |drawer| self.translated(drawer),
        );
        let layer =
            Row::with_children(vec![space::horizontal().width(Length::Fill).into(), drawer])
                .height(Length::Fill)
                .into();
        (space::horizontal().width(slot).into(), layer)
    }

    /// The key of the running (or last) slide's drawer track.
    #[cfg(test)]
    const fn key(&self) -> MotionKey {
        self.drawer_key
    }
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
        let mut slide = DrawerSlide::new();
        assert!(!slide.starts_slide(true), "nothing observed yet");

        slide.sync(true, false, 400.0, true);

        assert!(!slide.is_moving());
        assert!(slide.column_slide().is_none());
    }

    #[test]
    fn opening_slides_in_from_the_edge_and_settles() {
        let mut slide = DrawerSlide::new();
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);

        assert!(slide.starts_slide(true));
        slide.sync(true, false, 400.0, true);

        assert!(slide.is_moving());
        assert_eq!(slide.path(), SlidePath::Columns);
        assert!(close(slide.drawer.get().x, 400.0), "starts off-screen");
        assert!(close(slide.drawer.target().x, 0.0));
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
        let mut slide = DrawerSlide::new();
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(true, false, 0.0, false);

        slide.sync(false, false, 400.0, true);

        assert!(slide.is_moving());
        assert!(close(slide.drawer.get().x, 0.0));
        assert!(close(slide.drawer.target().x, 400.0));
        let columns = slide.column_slide().expect("a column slide is running");
        assert!(close(columns.offset.get().x, -400.0));
        assert!(close(columns.offset.target().x, 0.0));

        let _ = clock.run_until_settled();
        assert!(!slide.is_moving());
    }

    #[test]
    fn turning_round_mid_slide_keeps_the_track_path_and_extent() {
        let mut slide = DrawerSlide::new();
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
        assert!(close(slide.drawer.target().x, 400.0));
        assert!(close(slide.columns.target().x, 0.0));
        // It carries on from part-way, not from the edge.
        let x = slide.drawer.get().x;
        assert!(x > 0.0 && x < 400.0, "restarted at {x}");
        let c = slide.columns.get().x;
        assert!(c < 0.0 && c > -400.0, "columns restarted at {c}");
    }

    #[test]
    fn the_columns_path_needs_a_fit_and_no_condensing() {
        let mut no_fit = DrawerSlide::new();
        no_fit.sync(false, false, 0.0, false);
        no_fit.sync(true, false, 400.0, false);
        assert_eq!(no_fit.path(), SlidePath::Snap);
        assert!(no_fit.column_slide().is_none());

        let mut condenses = DrawerSlide::new();
        condenses.sync(false, false, 0.0, false);
        condenses.sync(true, true, 400.0, true);
        assert_eq!(condenses.path(), SlidePath::Snap);

        let mut was_condensed = DrawerSlide::new();
        was_condensed.sync(true, true, 0.0, false);
        was_condensed.sync(false, false, 400.0, true);
        assert_eq!(was_condensed.path(), SlidePath::Snap);
    }

    #[test]
    fn an_unchanged_observation_does_nothing() {
        let mut slide = DrawerSlide::new();
        slide.sync(true, false, 0.0, false);
        assert!(!slide.starts_slide(true));
        slide.sync(true, false, 400.0, true);
        assert!(!slide.is_moving());
    }

    #[test]
    fn turning_round_twice_mid_slide_stays_on_one_track() {
        let mut slide = DrawerSlide::new();
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
        assert!(close(slide.drawer.target().x, 0.0));
        assert!(close(slide.columns.target().x, -400.0));
    }

    #[test]
    fn a_slide_after_rest_starts_afresh_at_the_new_width() {
        let mut slide = DrawerSlide::new();
        let mut clock = FrameClock::new(slide.motion());
        slide.sync(false, false, 0.0, false);
        slide.sync(true, false, 400.0, true);
        let _ = clock.run_until_settled();
        let key = slide.key();

        slide.sync(false, false, 300.0, false);

        assert_ne!(slide.key(), key, "a fresh slide from rest, a fresh track");
        assert_eq!(slide.path(), SlidePath::Snap);
        assert!(close(slide.drawer.get().x, 0.0), "starts at rest");

        let _ = clock.run_until_settled();
        assert!(close(slide.drawer.get().x, 300.0));
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
    fn window(slide: &DrawerSlide, shown: bool) -> Element<'static, ()> {
        use crate::ui::iced::Length;
        use crate::ui::widget::{Row, Stack, scrollable, space};

        let inline = shown && !(slide.is_moving() && slide.path() == SlidePath::Columns);
        let drawer = scrollable(space::vertical().height(Length::Fixed(400.0)))
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(100.0));
        let (slot, layer) = slide.place(Some(drawer.into()), inline, 100.0);
        Stack::with_children(vec![
            Row::with_children(vec![space::horizontal().width(Length::Fill).into(), slot]).into(),
            layer,
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

        let mut slide = DrawerSlide::new();
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
}
