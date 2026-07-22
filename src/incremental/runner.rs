use super::{
    BackendError, CancelOutcome, IncrementalCompletion, OperationId, OperationLifecycle,
    OperationStateError, OperationTracker, PollDisposition, WaitSet, WakeReason, WorkReport,
};

/// Round-robin selector shared by command, backend, L2, and timer work.
#[derive(Debug, Default)]
pub struct FairWakeSelector {
    next: u8,
}

impl FairWakeSelector {
    /// Construct a selector whose first choice is the command plane.
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    /// Select one subscribed ready source and rotate the next preference.
    pub fn select(&mut self, subscribed: WaitSet, ready: WaitSet) -> Option<WakeReason> {
        let eligible = subscribed.0 & ready.0;
        for offset in 0..WakeReason::COUNT {
            let index = (self.next + offset) % WakeReason::COUNT;
            let reason = WakeReason::from_index(index);
            if eligible & reason.bit() != 0 {
                self.next = (index + 1) % WakeReason::COUNT;
                return Some(reason);
            }
        }
        None
    }
}

/// Work requested by the deterministic runner core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunnerStep {
    /// No operation is active or a terminal result awaits collection.
    Idle,
    /// None of the subscribed sources is ready yet.
    Waiting(WaitSet),
    /// The command plane gets this fair turn.
    CommandReady(OperationId),
    /// Call [`super::IncrementalWifiBackend::poll`] once with this reason and budget.
    PollBackend {
        /// Operation to advance.
        operation: OperationId,
        /// Selected singleton wake reason.
        reason: WakeReason,
    },
}

/// Action required after a controller requests cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelDirective {
    /// A queued request never reached the backend and is already terminal.
    CompleteLocally,
    /// Notify the backend, then continue bounded polling until terminal.
    NotifyBackend,
    /// The same cancellation request was already recorded.
    AlreadyRequested,
}

/// State change accepted from one verified backend report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunnerTransition {
    /// The operation remains pending on these wake sources.
    Pending {
        /// Backend reported protocol-visible progress.
        made_progress: bool,
        /// Sources that may make another poll useful.
        wait_for: WaitSet,
    },
    /// The operation used its full budget and must yield before another poll.
    BudgetExhausted {
        /// Backend reported protocol-visible progress.
        made_progress: bool,
        /// Sources that may make another poll useful.
        wait_for: WaitSet,
    },
    /// One successful terminal result was committed.
    Completed(IncrementalCompletion),
    /// Cancellation became terminal.
    Cancelled {
        /// A successful completion arrived after cancellation and was suppressed.
        suppressed_completion: bool,
    },
    /// The backend rejected start or failed during bounded polling.
    Failed {
        /// Lossless backend failure.
        error: BackendError,
        /// Cancellation had already been requested when the backend failed.
        cancellation_pending: bool,
    },
}

/// Rejection of a deterministic runner transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunnerStateError {
    /// The operation lifecycle rejected the transition.
    Operation(OperationStateError),
    /// A report retained from an older operation generation was presented.
    StaleReport {
        /// Current operation identity.
        expected: OperationId,
        /// Identity carried by the backend report.
        actual: OperationId,
    },
}

impl From<OperationStateError> for RunnerStateError {
    fn from(value: OperationStateError) -> Self {
        Self::Operation(value)
    }
}

/// Chip-neutral state machine for a future [`crate::RadioRunner`].
///
/// This type neither owns nor calls a backend. The platform runner executes the
/// returned step, then feeds the bounded report back through [`Self::apply_report`].
/// Keeping that boundary explicit makes host interleavings deterministic while
/// the validated blocking WS63 backend remains the default implementation.
#[derive(Debug)]
pub struct IncrementalRunnerState {
    tracker: OperationTracker,
    selector: FairWakeSelector,
    wait_for: WaitSet,
}

impl IncrementalRunnerState {
    /// Construct an idle runner state.
    pub const fn new() -> Self {
        Self {
            tracker: OperationTracker::new(),
            selector: FairWakeSelector::new(),
            wait_for: WaitSet::empty(),
        }
    }

    /// Queue one serialized control operation.
    pub fn queue(&mut self, slot: u8) -> Result<OperationId, RunnerStateError> {
        let id = self.tracker.queue(slot)?;
        self.wait_for = WaitSet::COMMAND;
        Ok(id)
    }

    /// Record that the backend accepted a queued request.
    pub fn mark_started(&mut self, id: OperationId) -> Result<(), RunnerStateError> {
        self.tracker.mark_started(id)?;
        self.wait_for = WaitSet::BACKEND;
        Ok(())
    }

    /// Terminalize a queued request after [`super::IncrementalWifiBackend::start`] fails.
    pub fn reject_start(
        &mut self,
        id: OperationId,
        error: BackendError,
    ) -> Result<RunnerTransition, RunnerStateError> {
        self.tracker.reject_queued(id)?;
        self.wait_for = WaitSet::empty();
        Ok(RunnerTransition::Failed {
            error,
            cancellation_pending: false,
        })
    }

    /// Request cancellation without invoking the backend from this state machine.
    pub fn request_cancel(&mut self, id: OperationId) -> Result<CancelDirective, RunnerStateError> {
        let lifecycle = self.tracker.lifecycle(id)?;
        let outcome = self.tracker.request_cancel(id)?;
        if outcome == CancelOutcome::AlreadyRequested {
            return Ok(CancelDirective::AlreadyRequested);
        }
        if lifecycle == OperationLifecycle::Queued {
            self.tracker.commit_terminal(id)?;
            self.wait_for = WaitSet::empty();
            Ok(CancelDirective::CompleteLocally)
        } else {
            self.wait_for = self.wait_for.union(WaitSet::BACKEND);
            Ok(CancelDirective::NotifyBackend)
        }
    }

    /// Choose the next fair action from the currently ready sources.
    pub fn select_step(&mut self, ready: WaitSet) -> RunnerStep {
        let Some((operation, lifecycle)) = self.tracker.current() else {
            return RunnerStep::Idle;
        };
        if lifecycle == OperationLifecycle::Terminal {
            return RunnerStep::Idle;
        }
        if self.wait_for.is_empty()
            && matches!(
                lifecycle,
                OperationLifecycle::Started | OperationLifecycle::CancelRequested
            )
        {
            return RunnerStep::PollBackend {
                operation,
                reason: WakeReason::Backend,
            };
        }
        match self.selector.select(self.wait_for, ready) {
            Some(WakeReason::Command) => RunnerStep::CommandReady(operation),
            Some(reason) => RunnerStep::PollBackend { operation, reason },
            None => RunnerStep::Waiting(self.wait_for),
        }
    }

    /// Apply one bounded report after a [`RunnerStep::PollBackend`] action.
    pub fn apply_report(
        &mut self,
        expected: OperationId,
        report: WorkReport,
    ) -> Result<RunnerTransition, RunnerStateError> {
        if !matches!(
            self.tracker.lifecycle(expected)?,
            OperationLifecycle::Started | OperationLifecycle::CancelRequested
        ) {
            return Err(OperationStateError::InvalidTransition.into());
        }
        if report.operation() != expected {
            return Err(RunnerStateError::StaleReport {
                expected,
                actual: report.operation(),
            });
        }
        match report.disposition() {
            PollDisposition::Pending(wait_for) => {
                self.wait_for = wait_for;
                Ok(RunnerTransition::Pending {
                    made_progress: report.made_progress(),
                    wait_for,
                })
            }
            PollDisposition::BudgetExhausted(wait_for) => {
                self.wait_for = wait_for;
                Ok(RunnerTransition::BudgetExhausted {
                    made_progress: report.made_progress(),
                    wait_for,
                })
            }
            PollDisposition::Complete(completion) => {
                let cancelled = self.tracker.commit_terminal(expected)?;
                self.wait_for = WaitSet::empty();
                if cancelled {
                    Ok(RunnerTransition::Cancelled {
                        suppressed_completion: true,
                    })
                } else {
                    Ok(RunnerTransition::Completed(completion))
                }
            }
            PollDisposition::Cancelled => {
                self.tracker.commit_terminal(expected)?;
                self.wait_for = WaitSet::empty();
                Ok(RunnerTransition::Cancelled {
                    suppressed_completion: false,
                })
            }
        }
    }

    /// Terminalize an accepted operation after a backend `poll` or `cancel` error.
    pub fn apply_error(
        &mut self,
        expected: OperationId,
        error: BackendError,
    ) -> Result<RunnerTransition, RunnerStateError> {
        if !matches!(
            self.tracker.lifecycle(expected)?,
            OperationLifecycle::Started | OperationLifecycle::CancelRequested
        ) {
            return Err(OperationStateError::InvalidTransition.into());
        }
        let cancellation_pending = self.tracker.commit_terminal(expected)?;
        self.wait_for = WaitSet::empty();
        Ok(RunnerTransition::Failed {
            error,
            cancellation_pending,
        })
    }

    /// Release a collected terminal result so the slot may be reused.
    pub fn reap(&mut self, id: OperationId) -> Result<(), RunnerStateError> {
        self.tracker.reap(id)?;
        self.wait_for = WaitSet::empty();
        Ok(())
    }

    /// Current operation and lifecycle, if any.
    pub const fn current(&self) -> Option<(OperationId, OperationLifecycle)> {
        self.tracker.current()
    }

    /// Wake sources subscribed by the current operation.
    pub const fn wait_for(&self) -> WaitSet {
        self.wait_for
    }
}

impl Default for IncrementalRunnerState {
    fn default() -> Self {
        Self::new()
    }
}
