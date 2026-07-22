//! Experimental incremental backend contract.
//!
//! This module is deliberately feature-gated. It freezes the operation identity,
//! cancellation, work-budget, and wait-set vocabulary before the validated WS63
//! backend is migrated. It does not replace [`crate::WifiBackend`] yet.

use core::num::{NonZeroU16, NonZeroU32};

use crate::{
    BackendError, ConnectionInfo, ScanConfig, ScanOutcome, ScanResult, StationConfig, WifiConfig,
};

mod command;
mod driver;
mod runner;

pub use command::{
    CommandArbiter, CommandArbiterAction, CommandArbiterError, CommandSequence, PendingCommand,
    SubmitError,
};
pub use driver::{IncrementalBackendDriver, IncrementalDriverError, IncrementalDriverEvent};
pub use runner::{
    CancelDirective, FairWakeSelector, IncrementalRunnerState, RunnerStateError, RunnerStep,
    RunnerTransition,
};

/// Identity of one backend operation slot and its current generation.
///
/// Reusing a slot increments `generation`, so a completion retained from an
/// earlier operation cannot complete the new operation accidentally.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationId {
    slot: u8,
    generation: NonZeroU32,
}

impl OperationId {
    /// Operation slot selected by the runner.
    pub const fn slot(self) -> u8 {
        self.slot
    }

    /// Non-zero identity generation for this use of the slot.
    pub const fn generation(self) -> NonZeroU32 {
        self.generation
    }
}

/// Explicit lifecycle of the single operation tracked by a runner slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationLifecycle {
    /// Accepted by the runner but not yet handed to the backend.
    Queued,
    /// Accepted by the backend and eligible for bounded polling.
    Started,
    /// Cancellation was requested; only a cancelled or terminal result may follow.
    CancelRequested,
    /// One terminal result has been committed and awaits collection.
    Terminal,
}

/// Rejection of an operation lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStateError {
    /// The one operation slot is still owned by another live operation.
    Busy,
    /// The supplied identity refers to an older generation or another slot.
    Stale,
    /// The requested transition is not legal from the current lifecycle state.
    InvalidTransition,
    /// A terminal result has already been committed.
    AlreadyTerminal,
}

/// Whether a cancellation request changed the operation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    /// The operation moved to `CancelRequested`.
    Requested,
    /// Cancellation had already been requested for this operation.
    AlreadyRequested,
}

/// Small state machine used by the future incremental runner.
///
/// It intentionally tracks one slot: the public controller serializes control
/// operations today. Additional slots can be added without weakening generation
/// checks if a demonstrated use case appears.
#[derive(Debug)]
pub struct OperationTracker {
    next_generation: u32,
    current: Option<(OperationId, OperationLifecycle)>,
}

impl OperationTracker {
    /// Construct an idle tracker.
    pub const fn new() -> Self {
        Self {
            next_generation: 0,
            current: None,
        }
    }

    /// Queue a new operation in `slot` and allocate its identity generation.
    pub fn queue(&mut self, slot: u8) -> Result<OperationId, OperationStateError> {
        if self.current.is_some() {
            return Err(OperationStateError::Busy);
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        let id = OperationId {
            slot,
            generation: NonZeroU32::new(self.next_generation).expect("generation is non-zero"),
        };
        self.current = Some((id, OperationLifecycle::Queued));
        Ok(id)
    }

    /// Return the current operation and lifecycle, if the slot is occupied.
    pub const fn current(&self) -> Option<(OperationId, OperationLifecycle)> {
        self.current
    }

    /// Mark a queued operation as accepted by the backend.
    pub fn mark_started(&mut self, id: OperationId) -> Result<(), OperationStateError> {
        let lifecycle = self.lifecycle_mut(id)?;
        if *lifecycle != OperationLifecycle::Queued {
            return Err(OperationStateError::InvalidTransition);
        }
        *lifecycle = OperationLifecycle::Started;
        Ok(())
    }

    /// Request cancellation without committing a terminal result.
    pub fn request_cancel(
        &mut self,
        id: OperationId,
    ) -> Result<CancelOutcome, OperationStateError> {
        let lifecycle = self.lifecycle_mut(id)?;
        match *lifecycle {
            OperationLifecycle::Queued | OperationLifecycle::Started => {
                *lifecycle = OperationLifecycle::CancelRequested;
                Ok(CancelOutcome::Requested)
            }
            OperationLifecycle::CancelRequested => Ok(CancelOutcome::AlreadyRequested),
            OperationLifecycle::Terminal => Err(OperationStateError::AlreadyTerminal),
        }
    }

    /// Commit the operation's sole terminal result.
    ///
    /// The return value says whether cancellation was pending when the result
    /// became terminal. A runner must suppress a late success when it is `true`.
    pub fn commit_terminal(&mut self, id: OperationId) -> Result<bool, OperationStateError> {
        let lifecycle = self.lifecycle_mut(id)?;
        let cancelled = match *lifecycle {
            OperationLifecycle::Started => false,
            OperationLifecycle::CancelRequested => true,
            OperationLifecycle::Queued => return Err(OperationStateError::InvalidTransition),
            OperationLifecycle::Terminal => return Err(OperationStateError::AlreadyTerminal),
        };
        *lifecycle = OperationLifecycle::Terminal;
        Ok(cancelled)
    }

    /// Mark a queued request terminal when the backend rejects `start`.
    pub fn reject_queued(&mut self, id: OperationId) -> Result<(), OperationStateError> {
        let lifecycle = self.lifecycle_mut(id)?;
        if *lifecycle != OperationLifecycle::Queued {
            return Err(OperationStateError::InvalidTransition);
        }
        *lifecycle = OperationLifecycle::Terminal;
        Ok(())
    }

    /// Release a collected terminal operation so the slot may be reused.
    pub fn reap(&mut self, id: OperationId) -> Result<(), OperationStateError> {
        let lifecycle = self.lifecycle(id)?;
        if lifecycle != OperationLifecycle::Terminal {
            return Err(OperationStateError::InvalidTransition);
        }
        self.current = None;
        Ok(())
    }

    /// Return the lifecycle of `id`, rejecting stale identities.
    pub fn lifecycle(&self, id: OperationId) -> Result<OperationLifecycle, OperationStateError> {
        let Some((current, lifecycle)) = self.current else {
            return Err(OperationStateError::Stale);
        };
        if current != id {
            return Err(OperationStateError::Stale);
        }
        Ok(lifecycle)
    }

    fn lifecycle_mut(
        &mut self,
        id: OperationId,
    ) -> Result<&mut OperationLifecycle, OperationStateError> {
        let Some((current, lifecycle)) = self.current.as_mut() else {
            return Err(OperationStateError::Stale);
        };
        if *current != id {
            return Err(OperationStateError::Stale);
        }
        Ok(lifecycle)
    }
}

impl Default for OperationTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Upper bound granted to one incremental backend poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkBudget {
    max_events: NonZeroU16,
    max_time_us: NonZeroU32,
}

impl WorkBudget {
    /// Construct a non-empty event and elapsed-time budget.
    pub const fn try_new(max_events: u16, max_time_us: u32) -> Option<Self> {
        match (NonZeroU16::new(max_events), NonZeroU32::new(max_time_us)) {
            (Some(max_events), Some(max_time_us)) => Some(Self {
                max_events,
                max_time_us,
            }),
            _ => None,
        }
    }

    /// Maximum backend events that may be consumed by one poll.
    pub const fn max_events(self) -> NonZeroU16 {
        self.max_events
    }

    /// Maximum elapsed backend time, in microseconds, for one poll.
    pub const fn max_time_us(self) -> NonZeroU32 {
        self.max_time_us
    }
}

/// Wake sources that an idle radio runner may wait on together.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WaitSet(u8);

impl WaitSet {
    /// A controller command entered the command queue.
    pub const COMMAND: Self = Self(1 << 0);
    /// A backend callback or deferred IRQ made protocol work ready.
    pub const BACKEND: Self = Self(1 << 1);
    /// Link-layer receive work is ready.
    pub const L2_RX: Self = Self(1 << 2);
    /// The next backend deadline elapsed.
    pub const TIMER: Self = Self(1 << 3);

    /// An empty set, used when another poll should happen immediately.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Combine independent wake sources.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Test whether all sources in `other` are present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Stable machine-readable bit representation.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Construct the singleton set for one wake reason.
    pub const fn from_reason(reason: WakeReason) -> Self {
        Self(reason.bit())
    }

    /// Test whether the set has no wake sources.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Test whether the sets share at least one wake source.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// One wake source selected for the next bounded runner step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeReason {
    /// A controller command is waiting.
    Command,
    /// A backend callback or deferred IRQ made protocol work ready.
    Backend,
    /// Link-layer receive work is ready.
    L2Rx,
    /// The next backend deadline elapsed.
    Timer,
}

impl WakeReason {
    const COUNT: u8 = 4;

    const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Command,
            1 => Self::Backend,
            2 => Self::L2Rx,
            _ => Self::Timer,
        }
    }

    const fn index(self) -> u8 {
        match self {
            Self::Command => 0,
            Self::Backend => 1,
            Self::L2Rx => 2,
            Self::Timer => 3,
        }
    }

    const fn bit(self) -> u8 {
        1 << self.index()
    }
}

/// Request moved into an incremental backend.
#[derive(Debug)]
pub enum IncrementalRequest {
    /// Initialize the radio runtime.
    Initialize(WifiConfig),
    /// Start one bounded scan.
    Scan(ScanConfig),
    /// Associate and authorize one station.
    Connect(StationConfig),
    /// Disconnect the station interface.
    Disconnect(WifiConfig),
}

/// Typed successful terminal result from an incremental backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncrementalCompletion {
    /// Initialization completed.
    Initialized,
    /// Scan completed and wrote results into the runner-owned buffer.
    Scan(ScanOutcome),
    /// Association and authorization completed.
    Connected(ConnectionInfo),
    /// Disconnection completed.
    Disconnected,
}

/// State returned by one bounded backend poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollDisposition {
    /// No terminal result; wait for these sources before polling again.
    Pending(WaitSet),
    /// One successful terminal result is ready.
    Complete(IncrementalCompletion),
    /// Cancellation reached a terminal state.
    Cancelled,
    /// The poll consumed its granted budget and requires another fair turn.
    BudgetExhausted(WaitSet),
}

/// Verified accounting for one backend poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkReport {
    operation: OperationId,
    consumed_events: u16,
    elapsed_us: u32,
    made_progress: bool,
    disposition: PollDisposition,
}

impl WorkReport {
    /// Construct a report only when both budget dimensions were respected.
    pub const fn try_new(
        operation: OperationId,
        budget: WorkBudget,
        consumed_events: u16,
        elapsed_us: u32,
        made_progress: bool,
        disposition: PollDisposition,
    ) -> Option<Self> {
        if consumed_events > budget.max_events.get() || elapsed_us > budget.max_time_us.get() {
            return None;
        }
        if matches!(disposition, PollDisposition::BudgetExhausted(_))
            && consumed_events != budget.max_events.get()
            && elapsed_us != budget.max_time_us.get()
        {
            return None;
        }
        if matches!(disposition, PollDisposition::Pending(wait) if wait.is_empty())
            && !made_progress
        {
            return None;
        }
        Some(Self {
            operation,
            consumed_events,
            elapsed_us,
            made_progress,
            disposition,
        })
    }

    /// Operation identity advanced by this report.
    pub const fn operation(self) -> OperationId {
        self.operation
    }

    /// Number of protocol/backend events consumed by this poll.
    pub const fn consumed_events(self) -> u16 {
        self.consumed_events
    }

    /// Backend-measured elapsed time in microseconds.
    pub const fn elapsed_us(self) -> u32 {
        self.elapsed_us
    }

    /// Whether the backend changed protocol-visible state.
    pub const fn made_progress(self) -> bool {
        self.made_progress
    }

    /// Pending, complete, or cancelled result.
    pub const fn disposition(self) -> PollDisposition {
        self.disposition
    }
}

/// Opt-in bounded backend contract used by the A5B prototype.
///
/// `start`, `poll`, and `cancel` are called only by the unique radio runner.
/// Implementations must not invoke application callbacks. All long-running work
/// is advanced by repeated bounded `poll` calls.
pub trait IncrementalWifiBackend {
    /// Accept an operation identity and owned request.
    fn start(&mut self, id: OperationId, request: IncrementalRequest) -> Result<(), BackendError>;

    /// Advance one operation without exceeding `budget`.
    fn poll(
        &mut self,
        id: OperationId,
        reason: WakeReason,
        budget: WorkBudget,
        scan_output: &mut [ScanResult],
    ) -> Result<WorkReport, BackendError>;

    /// Request cancellation. Terminal cancellation is observed through `poll`.
    fn cancel(&mut self, id: OperationId) -> Result<(), BackendError>;

    /// Monotonic deadline for the next timer wake, in microseconds.
    fn next_deadline_us(&self, id: OperationId) -> Option<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeIncrementalBackend {
        active: Option<OperationId>,
        polls: u8,
    }

    impl IncrementalWifiBackend for FakeIncrementalBackend {
        fn start(
            &mut self,
            id: OperationId,
            _request: IncrementalRequest,
        ) -> Result<(), BackendError> {
            if self.active.replace(id).is_some() {
                return Err(BackendError::new(crate::BackendErrorClass::Busy, 1));
            }
            self.polls = 0;
            Ok(())
        }

        fn poll(
            &mut self,
            id: OperationId,
            _reason: WakeReason,
            budget: WorkBudget,
            _scan_output: &mut [ScanResult],
        ) -> Result<WorkReport, BackendError> {
            if self.active != Some(id) {
                return Err(BackendError::new(crate::BackendErrorClass::Other, 2));
            }
            self.polls += 1;
            let disposition = if self.polls == 1 {
                PollDisposition::Pending(WaitSet::BACKEND.union(WaitSet::TIMER))
            } else {
                self.active = None;
                PollDisposition::Complete(IncrementalCompletion::Initialized)
            };
            WorkReport::try_new(id, budget, 1, 10, true, disposition)
                .ok_or_else(|| BackendError::new(crate::BackendErrorClass::Other, 3))
        }

        fn cancel(&mut self, id: OperationId) -> Result<(), BackendError> {
            if self.active == Some(id) {
                self.active = None;
                Ok(())
            } else {
                Err(BackendError::new(crate::BackendErrorClass::Other, 2))
            }
        }

        fn next_deadline_us(&self, id: OperationId) -> Option<u64> {
            (self.active == Some(id)).then_some(1_000)
        }
    }

    #[test]
    fn stale_completion_cannot_commit_after_slot_reuse() {
        let mut tracker = OperationTracker::new();
        let first = tracker.queue(0).unwrap();
        tracker.mark_started(first).unwrap();
        assert!(!tracker.commit_terminal(first).unwrap());
        tracker.reap(first).unwrap();

        let second = tracker.queue(0).unwrap();
        assert_ne!(first.generation(), second.generation());
        assert_eq!(
            tracker.commit_terminal(first),
            Err(OperationStateError::Stale)
        );
        assert_eq!(
            tracker.current(),
            Some((second, OperationLifecycle::Queued))
        );
    }

    #[test]
    fn cancellation_is_idempotent_and_suppresses_late_success() {
        let mut tracker = OperationTracker::new();
        let id = tracker.queue(0).unwrap();
        tracker.mark_started(id).unwrap();
        assert_eq!(tracker.request_cancel(id), Ok(CancelOutcome::Requested));
        assert_eq!(
            tracker.request_cancel(id),
            Ok(CancelOutcome::AlreadyRequested)
        );
        assert!(tracker.commit_terminal(id).unwrap());
        assert_eq!(
            tracker.request_cancel(id),
            Err(OperationStateError::AlreadyTerminal)
        );
        tracker.reap(id).unwrap();
    }

    #[test]
    fn work_report_and_wait_set_enforce_both_budget_dimensions() {
        let budget = WorkBudget::try_new(3, 200).unwrap();
        let wait = WaitSet::COMMAND
            .union(WaitSet::BACKEND)
            .union(WaitSet::TIMER);
        let mut tracker = OperationTracker::new();
        let id = tracker.queue(0).unwrap();
        let report =
            WorkReport::try_new(id, budget, 3, 200, true, PollDisposition::Pending(wait)).unwrap();
        assert_eq!(report.operation(), id);
        assert_eq!(report.consumed_events(), 3);
        assert_eq!(report.elapsed_us(), 200);
        assert!(report.made_progress());
        assert!(wait.contains(WaitSet::COMMAND));
        assert!(wait.contains(WaitSet::BACKEND));
        assert!(!wait.contains(WaitSet::L2_RX));
        assert!(
            WorkReport::try_new(id, budget, 4, 1, true, PollDisposition::Pending(wait)).is_none()
        );
        assert!(
            WorkReport::try_new(id, budget, 1, 201, true, PollDisposition::Pending(wait)).is_none()
        );
        assert!(
            WorkReport::try_new(
                id,
                budget,
                1,
                1,
                true,
                PollDisposition::BudgetExhausted(wait),
            )
            .is_none()
        );
        assert!(
            WorkReport::try_new(
                id,
                budget,
                3,
                1,
                true,
                PollDisposition::BudgetExhausted(wait),
            )
            .is_some()
        );
        assert!(
            WorkReport::try_new(
                id,
                budget,
                0,
                0,
                false,
                PollDisposition::Pending(WaitSet::empty()),
            )
            .is_none()
        );
    }

    #[test]
    fn incremental_backend_advances_only_through_bounded_polling() {
        let mut tracker = OperationTracker::new();
        let mut backend = FakeIncrementalBackend {
            active: None,
            polls: 0,
        };
        let id = tracker.queue(0).unwrap();
        backend
            .start(id, IncrementalRequest::Initialize(WifiConfig::default()))
            .unwrap();
        tracker.mark_started(id).unwrap();

        let budget = WorkBudget::try_new(2, 50).unwrap();
        let mut scan_results = [ScanResult::EMPTY; 1];
        let first = backend
            .poll(id, WakeReason::Backend, budget, &mut scan_results)
            .unwrap();
        assert_eq!(
            first.disposition(),
            PollDisposition::Pending(WaitSet::BACKEND.union(WaitSet::TIMER))
        );
        let second = backend
            .poll(id, WakeReason::Timer, budget, &mut scan_results)
            .unwrap();
        assert_eq!(
            second.disposition(),
            PollDisposition::Complete(IncrementalCompletion::Initialized)
        );
        assert!(!tracker.commit_terminal(id).unwrap());
        tracker.reap(id).unwrap();
    }

    #[test]
    fn fair_selector_rotates_across_continuously_ready_sources() {
        let all = WaitSet::COMMAND
            .union(WaitSet::BACKEND)
            .union(WaitSet::L2_RX)
            .union(WaitSet::TIMER);
        let mut selector = FairWakeSelector::new();
        assert_eq!(selector.select(all, all), Some(WakeReason::Command));
        assert_eq!(selector.select(all, all), Some(WakeReason::Backend));
        assert_eq!(selector.select(all, all), Some(WakeReason::L2Rx));
        assert_eq!(selector.select(all, all), Some(WakeReason::Timer));
        assert_eq!(selector.select(all, all), Some(WakeReason::Command));
    }

    #[test]
    fn cancellation_before_start_completes_without_backend_notification() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        assert_eq!(
            runner.queue(1),
            Err(RunnerStateError::Operation(OperationStateError::Busy))
        );
        assert_eq!(
            runner.request_cancel(id),
            Ok(CancelDirective::CompleteLocally)
        );
        assert_eq!(runner.current(), Some((id, OperationLifecycle::Terminal)));
        assert_eq!(runner.select_step(WaitSet::COMMAND), RunnerStep::Idle);
        runner.reap(id).unwrap();
        assert_eq!(runner.current(), None);
    }

    #[test]
    fn queued_operation_rejects_a_backend_report() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        let budget = WorkBudget::try_new(1, 10).unwrap();
        let report = WorkReport::try_new(
            id,
            budget,
            1,
            10,
            true,
            PollDisposition::Complete(IncrementalCompletion::Initialized),
        )
        .unwrap();
        assert_eq!(
            runner.apply_report(id, report),
            Err(RunnerStateError::Operation(
                OperationStateError::InvalidTransition
            ))
        );
    }

    #[test]
    fn start_failure_terminalizes_and_releases_the_slot() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        let error = BackendError::new(crate::BackendErrorClass::Initialize, 7);
        assert_eq!(
            runner.reject_start(id, error),
            Ok(RunnerTransition::Failed {
                error,
                cancellation_pending: false,
            })
        );
        runner.reap(id).unwrap();
        assert!(runner.queue(0).is_ok());
    }

    #[test]
    fn poll_failure_preserves_whether_cancellation_was_pending() {
        let error = BackendError::new(crate::BackendErrorClass::Other, 9);

        let mut active = IncrementalRunnerState::new();
        let active_id = active.queue(0).unwrap();
        active.mark_started(active_id).unwrap();
        assert_eq!(
            active.apply_error(active_id, error),
            Ok(RunnerTransition::Failed {
                error,
                cancellation_pending: false,
            })
        );

        let mut cancelling = IncrementalRunnerState::new();
        let cancelling_id = cancelling.queue(0).unwrap();
        cancelling.mark_started(cancelling_id).unwrap();
        assert_eq!(
            cancelling.request_cancel(cancelling_id),
            Ok(CancelDirective::NotifyBackend)
        );
        assert_eq!(
            cancelling.apply_error(cancelling_id, error),
            Ok(RunnerTransition::Failed {
                error,
                cancellation_pending: true,
            })
        );
    }

    #[test]
    fn cancellation_after_start_suppresses_a_late_success() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        runner.mark_started(id).unwrap();
        assert_eq!(
            runner.request_cancel(id),
            Ok(CancelDirective::NotifyBackend)
        );
        let budget = WorkBudget::try_new(1, 10).unwrap();
        let report = WorkReport::try_new(
            id,
            budget,
            1,
            10,
            true,
            PollDisposition::Complete(IncrementalCompletion::Initialized),
        )
        .unwrap();
        assert_eq!(
            runner.apply_report(id, report),
            Ok(RunnerTransition::Cancelled {
                suppressed_completion: true,
            })
        );
    }

    #[test]
    fn stale_report_cannot_complete_a_reused_runner_slot() {
        let mut runner = IncrementalRunnerState::new();
        let first = runner.queue(0).unwrap();
        runner.mark_started(first).unwrap();
        let budget = WorkBudget::try_new(1, 10).unwrap();
        let first_report = WorkReport::try_new(
            first,
            budget,
            1,
            10,
            true,
            PollDisposition::Complete(IncrementalCompletion::Initialized),
        )
        .unwrap();
        runner.apply_report(first, first_report).unwrap();
        runner.reap(first).unwrap();

        let second = runner.queue(0).unwrap();
        runner.mark_started(second).unwrap();
        assert_eq!(
            runner.apply_report(second, first_report),
            Err(RunnerStateError::StaleReport {
                expected: second,
                actual: first,
            })
        );
        assert_eq!(
            runner.current(),
            Some((second, OperationLifecycle::Started))
        );
    }

    #[test]
    fn runner_selects_control_without_starving_backend_rx_or_timer() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        runner.mark_started(id).unwrap();
        let budget = WorkBudget::try_new(4, 100).unwrap();
        let all = WaitSet::COMMAND
            .union(WaitSet::BACKEND)
            .union(WaitSet::L2_RX)
            .union(WaitSet::TIMER);
        let subscribed =
            WorkReport::try_new(id, budget, 1, 10, true, PollDisposition::Pending(all)).unwrap();
        runner.apply_report(id, subscribed).unwrap();

        assert_eq!(runner.select_step(all), RunnerStep::CommandReady(id));
        assert_eq!(
            runner.select_step(all),
            RunnerStep::PollBackend {
                operation: id,
                reason: WakeReason::Backend,
            }
        );
        assert_eq!(
            runner.select_step(all),
            RunnerStep::PollBackend {
                operation: id,
                reason: WakeReason::L2Rx,
            }
        );
        assert_eq!(
            runner.select_step(all),
            RunnerStep::PollBackend {
                operation: id,
                reason: WakeReason::Timer,
            }
        );
        assert_eq!(runner.select_step(all), RunnerStep::CommandReady(id));
    }

    #[test]
    fn budget_exhaustion_requires_a_fair_follow_up_turn() {
        let mut runner = IncrementalRunnerState::new();
        let id = runner.queue(0).unwrap();
        runner.mark_started(id).unwrap();
        let budget = WorkBudget::try_new(2, 100).unwrap();
        let wait_for = WaitSet::BACKEND.union(WaitSet::TIMER);
        let report = WorkReport::try_new(
            id,
            budget,
            2,
            20,
            true,
            PollDisposition::BudgetExhausted(wait_for),
        )
        .unwrap();
        assert_eq!(
            runner.apply_report(id, report),
            Ok(RunnerTransition::BudgetExhausted {
                made_progress: true,
                wait_for,
            })
        );
        assert_eq!(
            runner.select_step(WaitSet::COMMAND),
            RunnerStep::Waiting(wait_for)
        );
        assert_eq!(
            runner.select_step(WaitSet::TIMER),
            RunnerStep::PollBackend {
                operation: id,
                reason: WakeReason::Timer,
            }
        );
    }
}
