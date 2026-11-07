//! Volume specialization of the generic keyed reconciliation controller.

use std::{rc::Rc, time::Duration};

use crate::{Error, controller};

use super::{SharedVolumeStore, VolumeId};

/// Observes recoverable volume reconciliation errors without stopping the controller.
pub type ErrorHandler = controller::ErrorHandler<VolumeId>;

/// A handle for requesting immediate volume convergence.
pub type Wakeup = controller::Wakeup<VolumeId>;

struct Source(SharedVolumeStore);

impl controller::Source<VolumeId> for Source {
    fn list_keys(&self) -> ::sandbox::LocalFuture<'_, Result<Vec<VolumeId>, Error>> {
        Box::pin(async move {
            self.0
                .list()
                .await
                .map(|records| records.into_iter().map(|record| record.id).collect())
        })
    }
}

/// Generic keyed reconciliation specialized for volumes.
pub struct Controller(controller::Controller<VolumeId>);

impl Controller {
    /// Creates a volume controller and its independently shareable wake-up handle.
    #[must_use]
    pub fn new(
        store: SharedVolumeStore,
        reconciler: Rc<dyn controller::Reconcile<VolumeId>>,
        interval: Duration,
        on_error: ErrorHandler,
    ) -> (Self, Wakeup) {
        let (controller, wakeup) =
            controller::Controller::new(Rc::new(Source(store)), reconciler, interval, "Volume", on_error);
        (Self(controller), wakeup)
    }

    /// Reconciles existing volumes immediately and then continuously.
    pub async fn run(self) {
        self.0.run().await;
    }
}
