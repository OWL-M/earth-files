//! The file operations the app started, from start to their row closed:
//! one record each, so that ending one, completed or failed, takes all it
//! had with it at once.

use std::collections::{BTreeMap, VecDeque};

use crate::mounter::{MounterItem, MounterKey};
use crate::operation::{Controller, Operation, OperationError, OperationSelection};

use super::{Question, Stall};

/// How many completed operations can be undone, newest first.
const UNDO_DEPTH: usize = 20;

#[derive(Default)]
pub(super) struct Operations {
    next_id: u64,
    running: BTreeMap<u64, Running>,
    /// Failed ones, with what is left of each to retry, until their row is
    /// closed or they are retried.
    failed: BTreeMap<u64, Failed>,
    /// Completed ones, without the payload they carried.
    complete: BTreeMap<u64, Operation>,
    /// What undoes each of the last completed operations, newest last.
    undo_stack: Vec<Vec<Operation>>,
}

/// A running operation, and what the app keeps about it meanwhile.
pub(super) struct Running {
    pub op: Operation,
    pub controller: Controller,
    /// The last question it asked about a path it may not touch. Dropping
    /// one still unanswered answers Cancel.
    pub question: Option<Question>,
    /// Since when its progress has not moved, once watched.
    pub stall: Option<Stall>,
    pub then: Then,
}

/// What follows an operation, given when it starts.
#[derive(Default)]
pub(super) struct Then {
    /// It is part of an undo: its own completion is not undoable again.
    pub undoing: bool,
    /// The parts of its undo still to come, each once the one before it
    /// completed: a restore needs its place free first.
    pub undo_rest: VecDeque<Operation>,
    /// A drive to eject once it completes, as emptying the drive's trash
    /// before unmounting does. Cancelled or failed, the drive stays.
    pub unmount: Option<(MounterKey, MounterItem)>,
}

/// A failed operation.
pub(super) struct Failed {
    /// What is left of it, for a retry.
    pub op: Operation,
    pub controller: Controller,
    pub reason: String,
}

/// An operation that just ended, as it was.
pub(super) struct Ended {
    pub op: Operation,
    pub controller: Controller,
    pub then: Then,
}

impl Operations {
    /// Starts keeping `op`, under a new id.
    pub fn start(&mut self, op: Operation, then: Then) -> (u64, Controller) {
        let id = self.next_id;
        self.next_id += 1;
        let controller = Controller::default();
        self.running.insert(
            id,
            Running {
                op,
                controller: controller.clone(),
                question: None,
                stall: None,
                then,
            },
        );
        (id, controller)
    }

    pub fn running(&self) -> impl DoubleEndedIterator<Item = (u64, &Running)> {
        self.running.iter().map(|(&id, running)| (id, running))
    }

    pub fn running_mut(&mut self) -> impl Iterator<Item = (u64, &mut Running)> {
        self.running.iter_mut().map(|(&id, running)| (id, running))
    }

    pub fn get(&self, id: u64) -> Option<&Running> {
        self.running.get(&id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Running> {
        self.running.get_mut(&id)
    }

    /// The running operation `controller` controls.
    pub fn id_of(&self, controller: &Controller) -> Option<u64> {
        self.running()
            .find(|(_, running)| running.controller.is_same(controller))
            .map(|(id, _)| id)
    }

    pub fn is_idle(&self) -> bool {
        self.running.is_empty()
    }

    /// Ends `id` as completed with `result`. Its undo is kept, unless it
    /// was part of one, and it is listed among the completed without its
    /// payload.
    pub fn complete(&mut self, id: u64, result: &OperationSelection) -> Option<Ended> {
        let Running {
            op,
            controller,
            then,
            ..
        } = self.running.remove(&id)?;
        if !then.undoing {
            self.keep_undo(op.undo(result));
        }
        let mut kept = op.clone();
        kept.release_payload();
        self.complete.insert(id, kept);
        Some(Ended {
            op,
            controller,
            then,
        })
    }

    /// Ends `id` as failed with `err`: what it did before is undoable on
    /// its own, and what is left of it is kept to retry. The parts of an
    /// undo after it are dropped, and so is a drive to eject.
    pub fn fail(&mut self, id: u64, err: &OperationError) -> Option<Ended> {
        let Running {
            op,
            controller,
            then,
            ..
        } = self.running.remove(&id)?;
        if !then.undoing {
            self.keep_undo(op.undo_after_failure(&err.partial));
        }
        // The payload stays: retrying a paste whose bytes were dropped
        // would write an empty file and call it a success
        self.failed.insert(
            id,
            Failed {
                op: op.remaining(&err.partial),
                controller: controller.clone(),
                reason: err.to_string(),
            },
        );
        Some(Ended {
            op,
            controller,
            then: Then::default(),
        })
    }

    pub fn failed(&self) -> impl DoubleEndedIterator<Item = (u64, &Failed)> {
        self.failed.iter().map(|(&id, failed)| (id, failed))
    }

    pub fn get_failed(&self, id: u64) -> Option<&Failed> {
        self.failed.get(&id)
    }

    pub fn get_failed_mut(&mut self, id: u64) -> Option<&mut Failed> {
        self.failed.get_mut(&id)
    }

    /// Stops keeping failed `id`, for a retry.
    pub fn take_failed(&mut self, id: u64) -> Option<Failed> {
        self.failed.remove(&id)
    }

    pub fn complete_ops(&self) -> impl DoubleEndedIterator<Item = &Operation> {
        self.complete.values()
    }

    fn keep_undo(&mut self, undo: Vec<Operation>) {
        if undo.is_empty() {
            return;
        }
        self.undo_stack.push(undo);
        if self.undo_stack.len() > UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
    }

    /// The newest undo kept, taken.
    pub fn pop_undo(&mut self) -> Option<Vec<Operation>> {
        self.undo_stack.pop()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Drops the undos `drop` picks.
    pub fn forget_undos(&mut self, drop: impl Fn(&[Operation]) -> bool) {
        self.undo_stack.retain(|undo| !drop(undo));
    }
}

#[cfg(test)]
mod tests {
    use super::{Operations, Then};
    use crate::operation::{Operation, OperationError, OperationSelection};
    use std::path::PathBuf;

    fn rename(from: &str, to: &str) -> Operation {
        Operation::Rename {
            from: PathBuf::from(from),
            to: PathBuf::from(to),
        }
    }

    #[test]
    fn a_completed_operation_goes_with_all_it_had_and_its_undo_is_kept() {
        let mut ops = Operations::default();
        let (id, controller) = ops.start(rename("/a", "/b"), Then::default());
        let (other, _) = ops.start(rename("/c", "/d"), Then::default());
        assert_ne!(id, other);
        assert_eq!(ops.id_of(&controller), Some(id));

        let ended = ops.complete(id, &OperationSelection::default()).unwrap();
        assert!(matches!(ended.op, Operation::Rename { .. }));
        assert!(ops.get(id).is_none() && ops.get(other).is_some());
        assert_eq!(ops.id_of(&controller), None);
        assert_eq!(ops.complete_ops().count(), 1);
        assert!(matches!(
            ops.pop_undo().as_deref(),
            Some([Operation::Rename { from, .. }]) if from == &PathBuf::from("/b")
        ));
        assert!(
            ops.complete(id, &OperationSelection::default()).is_none(),
            "ended once"
        );
    }

    #[test]
    fn an_undo_part_is_not_undoable_and_its_failure_drops_what_was_to_follow() {
        let mut ops = Operations::default();
        let then = Then {
            undoing: true,
            undo_rest: [rename("/x", "/y")].into(),
            unmount: None,
        };
        let (id, _) = ops.start(rename("/b", "/a"), then);
        let ended = ops.fail(id, &OperationError::from_msg("no")).unwrap();
        assert!(ended.then.undo_rest.is_empty(), "the rest does not follow");
        assert!(!ops.can_undo());
        let failed = ops.take_failed(id).unwrap();
        assert_eq!(failed.reason, "no");
        assert!(ops.is_idle() && ops.get_failed(id).is_none());

        let then = Then {
            undoing: true,
            undo_rest: [rename("/x", "/y")].into(),
            unmount: None,
        };
        let (id, _) = ops.start(rename("/b", "/a"), then);
        let ended = ops.complete(id, &OperationSelection::default()).unwrap();
        assert_eq!(ended.then.undo_rest.len(), 1, "the rest follows");
        assert!(!ops.can_undo(), "an undo is not undone in turn");
    }
}
