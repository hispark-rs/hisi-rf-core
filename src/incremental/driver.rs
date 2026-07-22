use crate::{BackendError, ScanResult};

use super::{
    CancelDirective, CommandArbiter, CommandArbiterAction, CommandArbiterError, CommandSequence,
    IncrementalCompletion, IncrementalRequest, IncrementalRunnerState, IncrementalWifiBackend,
    OperationId, PendingCommand, RunnerStateError, RunnerStep, RunnerTransition, SubmitError,
    WaitSet, WorkBudget,
};

/// Internal protocol failure while composing the incremental runner pieces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncrementalDriverError {
    /// The operation lifecycle rejected an unexpected transition.
    Runner(RunnerStateError),
    /// Active/pending command ownership rejected an unexpected transition.
    Arbiter(CommandArbiterError),
}

impl From<RunnerStateError> for IncrementalDriverError {
    fn from(value: RunnerStateError) -> Self {
        Self::Runner(value)
    }
}

impl From<CommandArbiterError> for IncrementalDriverError {
    fn from(value: CommandArbiterError) -> Self {
        Self::Arbiter(value)
    }
}

/// One bounded, externally observable driver transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncrementalDriverEvent {
    /// No active or queued operation exists.
    Idle,
    /// The backend accepted this operation.
    Started {
        /// Controller sequence that owns the operation.
        sequence: CommandSequence,
        /// Generation-tagged backend identity.
        operation: OperationId,
    },
    /// Cancellation was delivered to the active backend exactly once.
    CancelRequested {
        /// Controller sequence being cancelled.
        sequence: CommandSequence,
        /// Generation-tagged backend identity.
        operation: OperationId,
    },
    /// No subscribed wake source is currently ready.
    Waiting {
        /// Active controller sequence.
        sequence: CommandSequence,
        /// Generation-tagged backend identity.
        operation: OperationId,
        /// Wake sources that may make progress.
        wait_for: WaitSet,
    },
    /// One bounded poll made progress but remains pending.
    Pending {
        /// Active controller sequence.
        sequence: CommandSequence,
        /// Generation-tagged backend identity.
        operation: OperationId,
        /// Whether protocol-visible state changed.
        made_progress: bool,
        /// Wake sources subscribed for the next poll.
        wait_for: WaitSet,
    },
    /// One bounded poll consumed its complete work grant.
    BudgetExhausted {
        /// Active controller sequence.
        sequence: CommandSequence,
        /// Generation-tagged backend identity.
        operation: OperationId,
        /// Whether protocol-visible state changed.
        made_progress: bool,
        /// Wake sources subscribed for the next poll.
        wait_for: WaitSet,
    },
    /// The active request completed successfully.
    Completed {
        /// Controller sequence receiving the completion.
        sequence: CommandSequence,
        /// Typed backend result.
        completion: IncrementalCompletion,
    },
    /// Cancellation reached a terminal state.
    Cancelled {
        /// Controller sequence receiving the cancellation.
        sequence: CommandSequence,
        /// Whether a simultaneous successful completion was suppressed.
        suppressed_completion: bool,
    },
    /// Backend start, poll, or cancellation failed and the slot was reaped.
    Failed {
        /// Controller sequence receiving the failure.
        sequence: CommandSequence,
        /// Lossless backend failure.
        error: BackendError,
        /// Whether cancellation had already been requested.
        cancellation_pending: bool,
    },
}

/// Executable, deterministic composition of the A5B incremental contracts.
///
/// This driver deliberately does not own an async executor or the default
/// [`crate::RadioRunner`]. The opt-in [`crate::IncrementalRadioRunner`] facade
/// feeds commands and platform-derived wake bits into [`Self::drive_once`].
/// Keeping that boundary explicit lets host tests close cancellation,
/// stale-generation, error, and bounded-work semantics before any WS63 backend
/// becomes incremental.
pub struct IncrementalBackendDriver<B> {
    backend: B,
    arbiter: CommandArbiter<IncrementalRequest>,
    runner: IncrementalRunnerState,
    budget: WorkBudget,
}

impl<B: IncrementalWifiBackend> IncrementalBackendDriver<B> {
    /// Construct an idle driver with a fixed per-poll work grant.
    pub const fn new(backend: B, budget: WorkBudget) -> Self {
        Self {
            backend,
            arbiter: CommandArbiter::new(),
            runner: IncrementalRunnerState::new(),
            budget,
        }
    }

    /// Queue one request without overwriting an existing replacement request.
    pub fn submit(
        &mut self,
        sequence: CommandSequence,
        request: IncrementalRequest,
    ) -> Result<(), SubmitError<IncrementalRequest>> {
        self.arbiter.submit(PendingCommand::new(sequence, request))
    }

    /// Whether the bounded command arbiter can retain one more request.
    pub const fn can_submit(&self) -> bool {
        self.arbiter.can_submit()
    }

    /// Execute at most one start, cancellation, or bounded backend poll.
    pub fn drive_once(
        &mut self,
        ready: WaitSet,
        scan_output: &mut [ScanResult],
    ) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        match self.arbiter.action() {
            CommandArbiterAction::Idle => Ok(IncrementalDriverEvent::Idle),
            CommandArbiterAction::StartPending(_) => self.start_pending(),
            CommandArbiterAction::Starting(_) => Err(CommandArbiterError::InvalidTransition.into()),
            CommandArbiterAction::CancelActive(operation) => self.cancel_active(operation),
            CommandArbiterAction::WaitActive(operation) => {
                self.poll_active(operation, ready, scan_output)
            }
        }
    }

    /// Monotonic backend deadline for the active operation.
    pub fn next_deadline_us(&self) -> Option<u64> {
        let (_, operation) = self.arbiter.active()?;
        self.backend.next_deadline_us(operation)
    }

    /// Borrow the backend for platform wake registration and diagnostics.
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Recover the backend after the experimental driver is stopped.
    pub fn into_backend(self) -> B {
        self.backend
    }

    fn start_pending(&mut self) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        let pending = self.arbiter.take_startable()?;
        let sequence = pending.sequence();
        let operation = self.runner.queue(0)?;
        match self.backend.start(operation, pending.into_inner()) {
            Ok(()) => {
                self.runner.mark_started(operation)?;
                self.arbiter.mark_started(sequence, operation)?;
                Ok(IncrementalDriverEvent::Started {
                    sequence,
                    operation,
                })
            }
            Err(error) => {
                let transition = self.runner.reject_start(operation, error)?;
                self.runner.reap(operation)?;
                self.arbiter.reject_start(sequence)?;
                self.failed_event(sequence, transition)
            }
        }
    }

    fn cancel_active(
        &mut self,
        operation: OperationId,
    ) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        let (sequence, active) = self.active()?;
        if active != operation {
            return Err(CommandArbiterError::StaleOperation.into());
        }
        match self.runner.request_cancel(operation)? {
            CancelDirective::CompleteLocally => Err(CommandArbiterError::InvalidTransition.into()),
            CancelDirective::AlreadyRequested => Err(CommandArbiterError::InvalidTransition.into()),
            CancelDirective::NotifyBackend => match self.backend.cancel(operation) {
                Ok(()) => {
                    self.arbiter.mark_cancel_requested(operation)?;
                    Ok(IncrementalDriverEvent::CancelRequested {
                        sequence,
                        operation,
                    })
                }
                Err(error) => {
                    let transition = self.runner.apply_error(operation, error)?;
                    self.finish_terminal(operation, sequence, transition)
                }
            },
        }
    }

    fn poll_active(
        &mut self,
        operation: OperationId,
        ready: WaitSet,
        scan_output: &mut [ScanResult],
    ) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        let (sequence, active) = self.active()?;
        if active != operation {
            return Err(CommandArbiterError::StaleOperation.into());
        }
        match self.runner.select_step(ready) {
            RunnerStep::Idle | RunnerStep::CommandReady(_) => {
                Err(CommandArbiterError::InvalidTransition.into())
            }
            RunnerStep::Waiting(wait_for) => Ok(IncrementalDriverEvent::Waiting {
                sequence,
                operation,
                wait_for,
            }),
            RunnerStep::PollBackend { operation, reason } => {
                match self
                    .backend
                    .poll(operation, reason, self.budget, scan_output)
                {
                    Ok(report) => {
                        let transition = self.runner.apply_report(operation, report)?;
                        match transition {
                            RunnerTransition::Pending {
                                made_progress,
                                wait_for,
                            } => Ok(IncrementalDriverEvent::Pending {
                                sequence,
                                operation,
                                made_progress,
                                wait_for,
                            }),
                            RunnerTransition::BudgetExhausted {
                                made_progress,
                                wait_for,
                            } => Ok(IncrementalDriverEvent::BudgetExhausted {
                                sequence,
                                operation,
                                made_progress,
                                wait_for,
                            }),
                            transition => self.finish_terminal(operation, sequence, transition),
                        }
                    }
                    Err(error) => {
                        let transition = self.runner.apply_error(operation, error)?;
                        self.finish_terminal(operation, sequence, transition)
                    }
                }
            }
        }
    }

    fn finish_terminal(
        &mut self,
        operation: OperationId,
        sequence: CommandSequence,
        transition: RunnerTransition,
    ) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        let finished_sequence = self.arbiter.finish_active(operation)?;
        if finished_sequence != sequence {
            return Err(CommandArbiterError::StaleOperation.into());
        }
        self.runner.reap(operation)?;
        match transition {
            RunnerTransition::Completed(completion) => Ok(IncrementalDriverEvent::Completed {
                sequence,
                completion,
            }),
            RunnerTransition::Cancelled {
                suppressed_completion,
            } => Ok(IncrementalDriverEvent::Cancelled {
                sequence,
                suppressed_completion,
            }),
            transition @ RunnerTransition::Failed { .. } => self.failed_event(sequence, transition),
            RunnerTransition::Pending { .. } | RunnerTransition::BudgetExhausted { .. } => {
                Err(CommandArbiterError::InvalidTransition.into())
            }
        }
    }

    fn failed_event(
        &self,
        sequence: CommandSequence,
        transition: RunnerTransition,
    ) -> Result<IncrementalDriverEvent, IncrementalDriverError> {
        let RunnerTransition::Failed {
            error,
            cancellation_pending,
        } = transition
        else {
            return Err(CommandArbiterError::InvalidTransition.into());
        };
        Ok(IncrementalDriverEvent::Failed {
            sequence,
            error,
            cancellation_pending,
        })
    }

    fn active(&self) -> Result<(CommandSequence, OperationId), IncrementalDriverError> {
        self.arbiter
            .active()
            .ok_or(CommandArbiterError::InvalidTransition.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BackendErrorClass, IncrementalCompletion, PollDisposition, WifiConfig, WorkReport,
    };

    #[derive(Clone, Copy)]
    enum PollBehavior {
        Complete(IncrementalCompletion),
        Pending(WaitSet),
        Error(BackendError),
    }

    struct FakeBackend {
        fail_next_start: bool,
        cancel_error: Option<BackendError>,
        cancel_calls: u8,
        poll_behavior: PollBehavior,
    }

    impl FakeBackend {
        fn complete(completion: IncrementalCompletion) -> Self {
            Self {
                fail_next_start: false,
                cancel_error: None,
                cancel_calls: 0,
                poll_behavior: PollBehavior::Complete(completion),
            }
        }
    }

    impl IncrementalWifiBackend for FakeBackend {
        fn start(
            &mut self,
            _id: OperationId,
            _request: IncrementalRequest,
        ) -> Result<(), BackendError> {
            if self.fail_next_start {
                self.fail_next_start = false;
                Err(error(BackendErrorClass::Initialize, 1))
            } else {
                Ok(())
            }
        }

        fn poll(
            &mut self,
            id: OperationId,
            _reason: super::super::WakeReason,
            budget: WorkBudget,
            _scan_output: &mut [ScanResult],
        ) -> Result<WorkReport, BackendError> {
            match self.poll_behavior {
                PollBehavior::Complete(completion) => Ok(WorkReport::try_new(
                    id,
                    budget,
                    1,
                    1,
                    true,
                    PollDisposition::Complete(completion),
                )
                .unwrap()),
                PollBehavior::Pending(wait_for) => Ok(WorkReport::try_new(
                    id,
                    budget,
                    1,
                    1,
                    true,
                    PollDisposition::Pending(wait_for),
                )
                .unwrap()),
                PollBehavior::Error(error) => Err(error),
            }
        }

        fn cancel(&mut self, _id: OperationId) -> Result<(), BackendError> {
            self.cancel_calls += 1;
            match self.cancel_error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn next_deadline_us(&self, _id: OperationId) -> Option<u64> {
            Some(42)
        }
    }

    fn command_sequence(raw: u32) -> CommandSequence {
        CommandSequence::try_from_raw(raw).unwrap()
    }

    fn budget() -> WorkBudget {
        WorkBudget::try_new(4, 100).unwrap()
    }

    fn error(class: BackendErrorClass, code: u32) -> BackendError {
        BackendError::new(class, code)
    }

    fn empty_scan_output() -> [ScanResult; 1] {
        [ScanResult::empty(); 1]
    }

    #[test]
    fn replacement_cancels_once_and_suppresses_late_success() {
        let backend = FakeBackend::complete(IncrementalCompletion::Initialized);
        let mut driver = IncrementalBackendDriver::new(backend, budget());
        let mut output = empty_scan_output();

        driver
            .submit(
                command_sequence(1),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        let IncrementalDriverEvent::Started {
            operation: first, ..
        } = driver.drive_once(WaitSet::empty(), &mut output).unwrap()
        else {
            panic!("first request did not start");
        };
        driver
            .submit(
                command_sequence(2),
                IncrementalRequest::Disconnect(WifiConfig::default()),
            )
            .unwrap();
        assert_eq!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::CancelRequested {
                sequence: command_sequence(1),
                operation: first,
            }
        );
        assert_eq!(driver.backend().cancel_calls, 1);
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Waiting { .. }
        ));
        assert_eq!(driver.backend().cancel_calls, 1);
        assert_eq!(
            driver.drive_once(WaitSet::BACKEND, &mut output).unwrap(),
            IncrementalDriverEvent::Cancelled {
                sequence: command_sequence(1),
                suppressed_completion: true,
            }
        );
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Started {
                sequence,
                operation: _
            } if sequence == command_sequence(2)
        ));
    }

    #[test]
    fn failed_start_is_reaped_before_next_command() {
        let mut backend = FakeBackend::complete(IncrementalCompletion::Initialized);
        backend.fail_next_start = true;
        let mut driver = IncrementalBackendDriver::new(backend, budget());
        let mut output = empty_scan_output();
        driver
            .submit(
                command_sequence(1),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Failed {
                sequence,
                cancellation_pending: false,
                ..
            } if sequence == command_sequence(1)
        ));
        driver
            .submit(
                command_sequence(2),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Started { sequence, .. } if sequence == command_sequence(2)
        ));
    }

    #[test]
    fn failed_cancellation_is_terminal_before_replacement_starts() {
        let cancel_error = error(BackendErrorClass::Other, 7);
        let mut backend = FakeBackend::complete(IncrementalCompletion::Initialized);
        backend.cancel_error = Some(cancel_error);
        let mut driver = IncrementalBackendDriver::new(backend, budget());
        let mut output = empty_scan_output();
        driver
            .submit(
                command_sequence(1),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        let _ = driver.drive_once(WaitSet::empty(), &mut output).unwrap();
        driver
            .submit(
                command_sequence(2),
                IncrementalRequest::Disconnect(WifiConfig::default()),
            )
            .unwrap();
        assert_eq!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Failed {
                sequence: command_sequence(1),
                error: cancel_error,
                cancellation_pending: true,
            }
        );
        assert_eq!(driver.backend().cancel_calls, 1);
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Started { sequence, .. }
                if sequence == command_sequence(2)
        ));
    }

    #[test]
    fn poll_error_is_terminal_and_does_not_poison_reuse() {
        let backend_error = error(BackendErrorClass::Timeout, 9);
        let backend = FakeBackend {
            fail_next_start: false,
            cancel_error: None,
            cancel_calls: 0,
            poll_behavior: PollBehavior::Error(backend_error),
        };
        let mut driver = IncrementalBackendDriver::new(backend, budget());
        let mut output = empty_scan_output();
        driver
            .submit(
                command_sequence(1),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        let _ = driver.drive_once(WaitSet::empty(), &mut output).unwrap();
        assert_eq!(
            driver.drive_once(WaitSet::BACKEND, &mut output).unwrap(),
            IncrementalDriverEvent::Failed {
                sequence: command_sequence(1),
                error: backend_error,
                cancellation_pending: false,
            }
        );
        driver
            .submit(
                command_sequence(2),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        assert!(matches!(
            driver.drive_once(WaitSet::empty(), &mut output).unwrap(),
            IncrementalDriverEvent::Started { sequence, .. } if sequence == command_sequence(2)
        ));
    }

    #[test]
    fn pending_report_retains_wait_set_and_deadline() {
        let backend = FakeBackend {
            fail_next_start: false,
            cancel_error: None,
            cancel_calls: 0,
            poll_behavior: PollBehavior::Pending(WaitSet::L2_RX.union(WaitSet::TIMER)),
        };
        let mut driver = IncrementalBackendDriver::new(backend, budget());
        let mut output = empty_scan_output();
        driver
            .submit(
                command_sequence(1),
                IncrementalRequest::Initialize(WifiConfig::default()),
            )
            .unwrap();
        let _ = driver.drive_once(WaitSet::empty(), &mut output).unwrap();
        assert_eq!(driver.next_deadline_us(), Some(42));
        assert!(matches!(
            driver.drive_once(WaitSet::BACKEND, &mut output).unwrap(),
            IncrementalDriverEvent::Pending {
                wait_for,
                made_progress: true,
                ..
            } if wait_for == WaitSet::L2_RX.union(WaitSet::TIMER)
        ));
        assert!(matches!(
            driver.drive_once(WaitSet::BACKEND, &mut output).unwrap(),
            IncrementalDriverEvent::Waiting { wait_for, .. }
                if wait_for == WaitSet::L2_RX.union(WaitSet::TIMER)
        ));
    }
}
