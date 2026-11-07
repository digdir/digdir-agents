//! Progress of the copies volumes take.
//!
//! The volume reconciler folds the events of each copy into its volume's [`Provisioning`], which
//! readers see on the revision it copies. It lives in memory only, like an agent's provisioning:
//! after a daemon restart no copy is running, and a revision's state is its durable outcome.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use ::sandbox::{Phase, ProgressReporter};

use crate::{
    progress::{Provisioning, ProvisioningState},
    resources::Changes,
};

use super::VolumeId;

/// Checking the source before it is copied, for secret files and for what it holds.
pub const CHECK_SOURCE: Phase = Phase::new("volumeCheck", "Check source");
/// Copying the source into the volume's storage.
pub const COPY_SOURCE: Phase = Phase::new("volumeCopy", "Copy source");
/// Checking that the source did not change while it was copied.
pub const VERIFY_SOURCE: Phase = Phase::new("volumeVerify", "Check source is unchanged");

/// Progress of every volume's latest copy, shared by the volume reconciler and readers.
///
/// A volume takes one copy at a time, so it keeps only its latest, with the number of the revision
/// it copies.
#[derive(Clone, Default)]
pub struct CopyProgress {
    copies: ProvisioningState<VolumeId>,
    numbers: Rc<RefCell<HashMap<VolumeId, u64>>>,
}

impl CopyProgress {
    /// Creates empty state whose every change advances `changes`, so watchers see a copy advance.
    #[must_use]
    pub fn new(changes: Changes) -> Self {
        Self {
            copies: ProvisioningState::new(changes),
            numbers: Rc::default(),
        }
    }

    /// Returns the progress of copying revision `number` of the volume `id`, while it is the
    /// latest copy the volume took since agentd started.
    #[must_use]
    pub fn get(&self, id: VolumeId, number: u64) -> Option<Provisioning> {
        (self.numbers.borrow().get(&id) == Some(&number))
            .then(|| self.copies.get(id))
            .flatten()
    }

    /// Starts a copy of revision `number`, replacing the volume's previous one, and returns the
    /// reporter for its progress.
    pub(crate) fn begin(&self, id: VolumeId, number: u64) -> ProgressReporter {
        self.numbers.borrow_mut().insert(id, number);
        self.copies.begin(id);
        let copies = self.copies.clone();
        ProgressReporter::from_callback(move |event| copies.apply(id, &event))
    }

    pub(crate) fn succeed(&self, id: VolumeId) {
        self.copies.succeed(id);
    }

    pub(crate) fn fail(&self, id: VolumeId, detail: &str) {
        self.copies.fail(id, detail);
    }

    /// Drops a removed volume's progress.
    pub(crate) fn forget(&self, id: VolumeId) {
        self.numbers.borrow_mut().remove(&id);
        self.copies.forget(id);
    }
}
