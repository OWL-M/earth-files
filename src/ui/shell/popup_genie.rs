// SPDX-License-Identifier: GPL-3.0-only

//! What a popup needs to collapse into its anchor, and when to let it go.
//!
//! One entry per animated popup surface. The shell keeps this beside
//! `popup_views` and consults it in `view`; the decisions live here rather
//! than in `Shell` so they can be tested without an `Application`.

use std::collections::HashMap;
use std::time::Duration;

use iced_core::window;
use iced_texture_cache::iced_animate::{Curve, Easing, Motion, MotionKey};
use iced_texture_cache::{Corner, GenieShape, TextureCache};

/// How long a menu takes to collapse into the pointer, or to unfurl out of
/// it.
///
/// The one number here that is taste rather than arithmetic.
/// `GenieWarpMesh` uses 500ms, but that is a whole window travelling to a
/// dock rather than a small menu collapsing into the pointer beside it.
const DURATION: Duration = Duration::from_millis(140);

/// The progress curve, deliberately linear.
///
/// The genie carries all of its own easing — the stretch and squash phases,
/// the cubic side curve, the exponential row lag — and every one of those
/// was calibrated against the reference implementations as a function of a
/// uniformly moving progress. An ease here would compose two curves and
/// change that timing.
const CURVE: Curve = Curve::ease(Easing::Linear, DURATION);

/// One animated popup.
pub(crate) struct PopupGenie {
    motion: Motion,
    cache: TextureCache,
    key: MotionKey,
    anchor: Corner,
    target_width: f32,
    exiting: bool,
}

impl PopupGenie {
    /// The engine this popup's animation runs on. The view wraps its content
    /// in `motion.host(..)`, which is what ticks it.
    pub(crate) const fn motion(&self) -> &Motion {
        &self.motion
    }

    /// Its texture, whose identity must not change between frames.
    pub(crate) fn cache(&self) -> TextureCache {
        self.cache.clone()
    }

    /// The track this popup's progress lives on.
    pub(crate) const fn key(&self) -> MotionKey {
        self.key
    }

    /// The corner it collapses into.
    // Read by the unit tests; `shape` reads the field directly.
    #[allow(dead_code)]
    pub(crate) const fn anchor(&self) -> Corner {
        self.anchor
    }

    /// Whether it is on its way out.
    // Read by the unit tests; `shape` reads the field directly.
    #[allow(dead_code)]
    pub(crate) const fn exiting(&self) -> bool {
        self.exiting
    }

    /// The shape it collapses with. Its corners are the `Cached`'s
    /// `border_radius`.
    pub(crate) fn shape(&self) -> GenieShape {
        GenieShape {
            anchor: self.anchor,
            target_width: self.target_width,
            ..GenieShape::default()
        }
    }

    /// How far along it is, resolved each frame.
    ///
    /// `enter` replays only the first time the key is seen, so calling it
    /// every frame unfurls the menu once. `retire` starts the exit and makes
    /// `Motion::presence` report `Exiting` and then `Gone`, which is what
    /// the wrapper widget waits for.
    pub(crate) fn progress(&self) -> iced_texture_cache::iced_animate::Anim<f32> {
        if self.exiting {
            self.motion.retire(self.key, CURVE, 0.0)
        } else {
            self.motion.enter(self.key, CURVE, 0.0, 1.0)
        }
    }
}

/// How wide a band a menu collapses into, as a fraction of its own width.
///
/// A fixed fraction gives a wider band to a wider menu, which is the wrong
/// way round: what the eye measures it against is the pointer it is
/// collapsing into, not the menu it came from. So the band is chosen in
/// pixels — one and a half cursors — and then expressed as a fraction of
/// whichever menu is actually collapsing.
///
/// The cursor's size is not something the Wayland runtime hands us:
/// `exwlshellev` loads its own theme at a hardcoded 23. `XCURSOR_SIZE` is
/// the conventional source, and 24 the conventional default, so a
/// compositor that sets neither still lands within a pixel of what it draws.
fn target_width(menu_width: f32) -> f32 {
    /// How many cursors wide the band should be.
    const CURSORS: f32 = 1.5;
    /// The cursor size to assume when nothing says otherwise.
    const ASSUMED_CURSOR: f32 = 24.0;
    /// Used when the menu's width is not known, so the fraction cannot be
    /// worked out at all.
    const FALLBACK: f32 = 0.08;

    if !menu_width.is_finite() || menu_width <= 0.0 {
        return FALLBACK;
    }

    let cursor = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|size| size.parse::<f32>().ok())
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(ASSUMED_CURSOR);

    (CURSORS * cursor / menu_width).clamp(0.0, 1.0)
}

/// Every animated popup the shell currently has open.
#[derive(Default)]
pub(crate) struct PopupGenies {
    entries: HashMap<window::Id, PopupGenie>,
}

impl PopupGenies {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Starts animating the popup `id`, collapsing toward `anchor`.
    ///
    /// `menu_width` is the popup's own width in logical pixels, which sets
    /// how wide a band it collapses into; see [`target_width`].
    pub(crate) fn insert(&mut self, id: window::Id, anchor: Corner, menu_width: f32) {
        self.entries.insert(
            id,
            PopupGenie {
                motion: Motion::new(),
                cache: TextureCache::new(),
                // One track per popup, so a key minted once here is enough
                // and never collides with another popup's.
                key: MotionKey::unique(),
                anchor,
                target_width: target_width(menu_width),
                exiting: false,
            },
        );
    }

    pub(crate) fn get(&self, id: window::Id) -> Option<&PopupGenie> {
        self.entries.get(&id)
    }

    /// Marks `id` as leaving.
    ///
    /// `true` means the caller must withhold the surface teardown until the
    /// collapse finishes. `false` means there is nothing to wait for —
    /// either the popup is not animated, or it is already on its way out and
    /// a teardown is already pending. Withholding twice would strand the
    /// surface for good.
    pub(crate) fn begin_exit(&mut self, id: window::Id) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };

        if entry.exiting {
            return false;
        }

        entry.exiting = true;
        true
    }

    pub(crate) fn remove(&mut self, id: window::Id) {
        self.entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> window::Id {
        // `Id::unique` is the only constructor; `n` just documents intent.
        let _ = n;
        window::Id::unique()
    }

    #[test]
    fn the_band_is_sized_against_the_pointer_not_the_menu() {
        // A wider menu gets a proportionally smaller fraction, so the band
        // is the same width on screen either way.
        let narrow = target_width(200.0);
        let wide = target_width(400.0);

        assert!(wide < narrow, "{wide} is not smaller than {narrow}");
        assert!(
            (narrow * 200.0 - wide * 400.0).abs() < 1e-3,
            "the band is not the same width on screen: {} vs {}",
            narrow * 200.0,
            wide * 400.0
        );
    }

    #[test]
    fn an_unknown_menu_width_falls_back() {
        assert!((target_width(0.0) - 0.08).abs() < f32::EPSILON);
        assert!((target_width(-10.0) - 0.08).abs() < f32::EPSILON);
        assert!((target_width(f32::NAN) - 0.08).abs() < f32::EPSILON);
    }

    #[test]
    fn a_menu_narrower_than_the_band_is_not_asked_to_exceed_itself() {
        assert!(target_width(4.0) <= 1.0);
    }

    #[test]
    fn a_popup_that_did_not_ask_for_it_gets_no_animation() {
        let mut genies = PopupGenies::new();
        let plain = id(1);

        assert!(genies.get(plain).is_none());
        // Nothing to wait for, so its teardown goes through immediately.
        assert!(!genies.begin_exit(plain));
    }

    #[test]
    fn an_animated_popup_defers_its_teardown_once() {
        let mut genies = PopupGenies::new();
        let menu = id(1);
        genies.insert(menu, Corner::TopLeft, 240.0);

        assert!(genies.get(menu).is_some());
        // The first teardown is withheld and starts the collapse...
        assert!(genies.begin_exit(menu));
        assert!(genies.get(menu).is_some_and(|entry| entry.exiting()));
        // ...and a second one must not withhold a second removal, or the
        // surface would never be torn down.
        assert!(!genies.begin_exit(menu));
    }

    #[test]
    fn forgetting_a_popup_is_harmless_twice() {
        let mut genies = PopupGenies::new();
        let menu = id(1);
        genies.insert(menu, Corner::TopLeft, 240.0);

        genies.remove(menu);
        assert!(genies.get(menu).is_none());
        genies.remove(menu);
        assert!(genies.get(menu).is_none());
    }

    #[test]
    fn popups_do_not_share_animation_state() {
        let mut genies = PopupGenies::new();
        let first = id(1);
        let second = id(2);
        genies.insert(first, Corner::TopLeft, 240.0);
        genies.insert(second, Corner::BottomRight, 240.0);

        assert!(genies.begin_exit(first));

        let second_entry = genies.get(second).expect("still open");
        assert!(
            !second_entry.exiting(),
            "one exit took another popup with it"
        );
        assert_eq!(second_entry.anchor(), Corner::BottomRight);
    }
}
