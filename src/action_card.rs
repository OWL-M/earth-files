// SPDX-License-Identifier: GPL-3.0-only

//! The card in the bottom-right corner that holds the active tab's
//! [`TabAction`], above the progress card.
//!
//! It has no timeout: it shows for as long as the active tab offers the
//! action. When the tab stops offering one, the last action is kept for
//! [`progress::LEAVE`] so the card has something to show while it slides
//! shut, then forgotten.
//!
//! Pure: the caller passes the time, so it can be tested without a clock.

use std::time::Instant;

use crate::progress;
use crate::tab::TabAction;
use crate::ui::widget::segmented_button::Entity;

/// What the card shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Shown {
    /// The tab the button sends its message to.
    pub(crate) entity: Entity,
    pub(crate) action: TabAction,
    /// No tab offers it any more: sliding shut, its button disabled.
    pub(crate) leaving: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ActionCard {
    /// The last action shown, with its tab, and when it stopped being
    /// offered.
    last: Option<(Entity, TabAction, Option<Instant>)>,
}

impl ActionCard {
    /// Follows `current`, the active tab and the action it offers now.
    pub(crate) fn sync(&mut self, current: Option<(Entity, TabAction)>, now: Instant) {
        match current {
            Some((entity, action)) => self.last = Some((entity, action, None)),
            None => {
                if let Some((_, _, left_at)) = &mut self.last {
                    let left_at = *left_at.get_or_insert(now);
                    if now >= left_at + progress::LEAVE {
                        self.last = None;
                    }
                }
            }
        }
    }

    /// What the card shows, if anything.
    pub(crate) fn shown(&self) -> Option<Shown> {
        self.last.map(|(entity, action, left_at)| Shown {
            entity,
            action,
            leaving: left_at.is_some(),
        })
    }

    /// When the card has slid shut and its action is to be forgotten:
    /// nothing else may happen then to prompt a [`Self::sync`].
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let (_, _, left_at) = self.last?;
        left_at
            .map(|at| at + progress::LEAVE)
            .filter(|&at| at > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trash() -> Option<(Entity, TabAction)> {
        Some((Entity::default(), TabAction::EmptyTrash(None)))
    }

    #[test]
    fn an_offered_action_is_shown_without_a_deadline() {
        let now = Instant::now();
        let mut card = ActionCard::default();
        card.sync(trash(), now);
        assert_eq!(
            card.shown(),
            Some(Shown {
                entity: Entity::default(),
                action: TabAction::EmptyTrash(None),
                leaving: false,
            })
        );
        assert_eq!(card.next_deadline(now), None);
    }

    #[test]
    fn nothing_is_shown_when_nothing_was_offered() {
        let mut card = ActionCard::default();
        card.sync(None, Instant::now());
        assert_eq!(card.shown(), None);
    }

    #[test]
    fn an_action_no_longer_offered_slides_shut_then_goes() {
        let now = Instant::now();
        let mut card = ActionCard::default();
        card.sync(trash(), now);
        card.sync(None, now);
        assert_eq!(card.shown().map(|shown| shown.leaving), Some(true));
        assert_eq!(card.next_deadline(now), Some(now + progress::LEAVE));

        // A sync before the slide ends keeps when it started
        card.sync(None, now + progress::LEAVE / 2);
        assert_eq!(card.next_deadline(now), Some(now + progress::LEAVE));

        card.sync(None, now + progress::LEAVE);
        assert_eq!(card.shown(), None);
        assert_eq!(card.next_deadline(now + progress::LEAVE), None);
    }

    #[test]
    fn an_action_offered_again_while_leaving_stays() {
        let now = Instant::now();
        let mut card = ActionCard::default();
        card.sync(trash(), now);
        card.sync(None, now);
        card.sync(trash(), now + progress::LEAVE / 2);
        assert_eq!(card.shown().map(|shown| shown.leaving), Some(false));
        assert_eq!(card.next_deadline(now + progress::LEAVE / 2), None);
    }

    #[test]
    fn another_action_replaces_the_shown_one_in_place() {
        let now = Instant::now();
        let mut card = ActionCard::default();
        card.sync(trash(), now);
        card.sync(
            Some((Entity::default(), TabAction::ClearRecents(None))),
            now,
        );
        assert_eq!(
            card.shown().map(|shown| (shown.action, shown.leaving)),
            Some((TabAction::ClearRecents(None), false))
        );
    }
}
