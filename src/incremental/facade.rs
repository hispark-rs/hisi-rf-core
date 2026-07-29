use core::{cell::Cell, future::poll_fn, task::Poll};

use crate::wifi::{Command, CommandKind, Completion, CompletionKind};
use crate::{
    BackendError, BackendErrorClass, RadioController, RadioState, WifiConfig, WifiEvent, WifiParts,
};

use super::{
    CommandArbiterError, CommandSequence, IncrementalBackendDriver, IncrementalCompletion,
    IncrementalDriverError, IncrementalDriverEvent, IncrementalRequest, IncrementalWaitError,
    IncrementalWaitIntent, IncrementalWaitPlatform, IncrementalWifiBackend, SubmitError, WaitSet,
    WorkBudget,
};

// A runner-local cancellation has no chip/backend status code to preserve.
const RUNNER_CANCELLED_CODE: u32 = 0;

/// Observational counters for the opt-in incremental runner.
///
/// Counters saturate at `u32::MAX`. They are intentionally local to the unique
/// runner and never participate in scheduling, wake delivery, or correctness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncrementalRunnerDiagnostics {
    /// Calls to [`IncrementalRadioRunner::run_once`].
    pub run_once_calls: u32,
    /// Calls that supplied command readiness.
    pub command_ready_batches: u32,
    /// Calls that supplied backend readiness.
    pub backend_ready_batches: u32,
    /// Calls that supplied L2 RX readiness.
    pub l2_rx_ready_batches: u32,
    /// Calls that supplied timer readiness.
    pub timer_ready_batches: u32,
    /// Wait futures that were first polled.
    pub wait_ready_calls: u32,
    /// Wait futures that returned a ready set.
    pub wait_ready_completions: u32,
    /// Wait futures that returned without an external wake because work was
    /// already runnable.
    pub immediate_ready_completions: u32,
    /// Wait futures that failed closed.
    pub wait_ready_errors: u32,
    /// Backend operations accepted by the incremental driver.
    pub operations_started: u32,
    /// Cancellation requests delivered to the backend.
    pub cancellations_requested: u32,
    /// Polls that remained pending within their work grant.
    pub pending_polls: u32,
    /// Polls that consumed their complete work grant.
    pub budget_exhaustions: u32,
    /// Operations that completed successfully.
    pub operations_completed: u32,
    /// Operations that reached cancelled terminal state.
    pub operations_cancelled: u32,
    /// Operations that reached failed terminal state.
    pub operations_failed: u32,
    /// Internal driver transition failures.
    pub driver_errors: u32,
    /// Facade publication or command-ledger failures.
    pub protocol_errors: u32,
}

impl IncrementalRunnerDiagnostics {
    const EMPTY: Self = Self {
        run_once_calls: 0,
        command_ready_batches: 0,
        backend_ready_batches: 0,
        l2_rx_ready_batches: 0,
        timer_ready_batches: 0,
        wait_ready_calls: 0,
        wait_ready_completions: 0,
        immediate_ready_completions: 0,
        wait_ready_errors: 0,
        operations_started: 0,
        cancellations_requested: 0,
        pending_polls: 0,
        budget_exhaustions: 0,
        operations_completed: 0,
        operations_cancelled: 0,
        operations_failed: 0,
        driver_errors: 0,
        protocol_errors: 0,
    };

    fn increment(counter: &mut u32) {
        *counter = counter.saturating_add(1);
    }

    fn record_ready(&mut self, ready: WaitSet) {
        if ready.contains(WaitSet::COMMAND) {
            Self::increment(&mut self.command_ready_batches);
        }
        if ready.contains(WaitSet::BACKEND) {
            Self::increment(&mut self.backend_ready_batches);
        }
        if ready.contains(WaitSet::L2_RX) {
            Self::increment(&mut self.l2_rx_ready_batches);
        }
        if ready.contains(WaitSet::TIMER) {
            Self::increment(&mut self.timer_ready_batches);
        }
    }

    fn record_event(&mut self, event: IncrementalDriverEvent) {
        match event {
            IncrementalDriverEvent::Started { .. } => {
                Self::increment(&mut self.operations_started);
            }
            IncrementalDriverEvent::CancelRequested { .. } => {
                Self::increment(&mut self.cancellations_requested);
            }
            IncrementalDriverEvent::Pending { .. } => {
                Self::increment(&mut self.pending_polls);
            }
            IncrementalDriverEvent::BudgetExhausted { .. } => {
                Self::increment(&mut self.budget_exhaustions);
            }
            IncrementalDriverEvent::Completed { .. } => {
                Self::increment(&mut self.operations_completed);
            }
            IncrementalDriverEvent::Cancelled { .. } => {
                Self::increment(&mut self.operations_cancelled);
            }
            IncrementalDriverEvent::Failed { .. } => {
                Self::increment(&mut self.operations_failed);
            }
            IncrementalDriverEvent::Idle | IncrementalDriverEvent::Waiting { .. } => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandKindTag {
    Initialize,
    Scan,
    Connect,
    Disconnect,
}

impl CommandKindTag {
    const fn from_command(command: &CommandKind) -> Self {
        match command {
            CommandKind::Initialize => Self::Initialize,
            CommandKind::Scan(_) => Self::Scan,
            CommandKind::Connect(_) => Self::Connect,
            CommandKind::Disconnect => Self::Disconnect,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CommandLedger {
    entries: [Option<(CommandSequence, CommandKindTag)>; 2],
}

impl CommandLedger {
    const fn new() -> Self {
        Self {
            entries: [None, None],
        }
    }

    fn insert(&mut self, sequence: CommandSequence, kind: CommandKindTag) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.is_none()) else {
            return false;
        };
        *entry = Some((sequence, kind));
        true
    }

    fn remove(&mut self, sequence: CommandSequence) -> Option<CommandKindTag> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| matches!(entry, Some((candidate, _)) if *candidate == sequence))?;
        entry.take().map(|(_, kind)| kind)
    }
}

/// Internal failure while adapting the incremental driver to the async facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncrementalRadioRunnerError {
    /// The executable backend driver rejected an internal transition.
    Driver(IncrementalDriverError),
    /// A command carried the reserved zero sequence.
    InvalidCommandSequence,
    /// More than the supported active plus pending commands were observed.
    CommandCapacity,
    /// The bounded command arbiter rejected a facade transition.
    CommandArbiter(CommandArbiterError),
    /// A terminal event did not have matching command metadata.
    MissingCommand,
    /// The backend returned a completion for another request kind.
    CompletionMismatch,
}

impl From<IncrementalDriverError> for IncrementalRadioRunnerError {
    fn from(value: IncrementalDriverError) -> Self {
        Self::Driver(value)
    }
}

/// Wi-Fi control/data parts plus the opt-in incremental runner.
pub struct IncrementalRadioParts<B, D, const EVENTS: usize> {
    /// Existing async Wi-Fi controller and L2 device contract.
    pub wifi: WifiParts<D, EVENTS>,
    /// Long-lived bounded backend runner.
    pub runner: IncrementalRadioRunner<B, EVENTS>,
}

impl<B: IncrementalWifiBackend, D, const EVENTS: usize> RadioController<B, D, EVENTS> {
    /// Split ownership into the existing Wi-Fi API and the experimental runner.
    ///
    /// The default [`Self::split`] path remains the validated blocking runner.
    /// Callers must feed the returned runner a platform-derived ready wait-set
    /// and yield between bounded calls.
    pub fn split_incremental(self, budget: WorkBudget) -> IncrementalRadioParts<B, D, EVENTS> {
        let (wifi, backend, config, state) = self.split_components();
        IncrementalRadioParts {
            wifi,
            runner: IncrementalRadioRunner {
                driver: IncrementalBackendDriver::new(backend, budget),
                config,
                state,
                ledger: CommandLedger::new(),
                diagnostics: Cell::new(IncrementalRunnerDiagnostics::EMPTY),
            },
        }
    }
}

/// Async-facade adapter for one bounded incremental backend.
pub struct IncrementalRadioRunner<B, const EVENTS: usize> {
    driver: IncrementalBackendDriver<B>,
    config: WifiConfig,
    state: &'static RadioState<EVENTS>,
    ledger: CommandLedger,
    diagnostics: Cell<IncrementalRunnerDiagnostics>,
}

impl<B: IncrementalWifiBackend, const EVENTS: usize> IncrementalRadioRunner<B, EVENTS> {
    /// Accept at most one command and execute at most one bounded driver action.
    pub fn run_once(
        &mut self,
        ready: WaitSet,
    ) -> Result<IncrementalDriverEvent, IncrementalRadioRunnerError> {
        let mut diagnostics = self.diagnostics.get();
        IncrementalRunnerDiagnostics::increment(&mut diagnostics.run_once_calls);
        diagnostics.record_ready(ready);
        self.diagnostics.set(diagnostics);

        if self.driver.can_submit()
            && let Ok(command) = self.state.shared.commands.try_receive()
            && let Err(error) = self.submit_command(command)
        {
            let mut diagnostics = self.diagnostics.get();
            IncrementalRunnerDiagnostics::increment(&mut diagnostics.protocol_errors);
            self.diagnostics.set(diagnostics);
            return Err(error);
        }

        if let Ok(raw_sequence) = self.state.shared.cancellations.try_receive() {
            let Some(sequence) = CommandSequence::try_from_raw(raw_sequence) else {
                let mut diagnostics = self.diagnostics.get();
                IncrementalRunnerDiagnostics::increment(&mut diagnostics.protocol_errors);
                self.diagnostics.set(diagnostics);
                return Err(IncrementalRadioRunnerError::InvalidCommandSequence);
            };
            if let Some(event) = self.driver.request_cancel(sequence)? {
                let mut diagnostics = self.diagnostics.get();
                diagnostics.record_event(event);
                self.diagnostics.set(diagnostics);
                self.publish_terminal(event)?;
                return Ok(event);
            }
        }

        // SAFETY: this unique runner is the only writer. A terminal scan
        // completion is signalled only after `drive_once` returns and releases
        // the mutable borrow, matching the blocking runner's ownership rule.
        let scan_output = unsafe { &mut *self.state.shared.scan_results_ptr() };
        let event = match self
            .driver
            .drive_once(ready.without(WaitSet::CANCEL), scan_output)
        {
            Ok(event) => event,
            Err(error) => {
                let mut diagnostics = self.diagnostics.get();
                IncrementalRunnerDiagnostics::increment(&mut diagnostics.driver_errors);
                self.diagnostics.set(diagnostics);
                return Err(error.into());
            }
        };
        let mut diagnostics = self.diagnostics.get();
        diagnostics.record_event(event);
        self.diagnostics.set(diagnostics);
        if let Err(error) = self.publish_terminal(event) {
            let mut diagnostics = self.diagnostics.get();
            IncrementalRunnerDiagnostics::increment(&mut diagnostics.protocol_errors);
            self.diagnostics.set(diagnostics);
            return Err(error);
        }
        Ok(event)
    }

    /// Monotonic deadline currently requested by the backend.
    pub fn next_deadline_us(&self) -> Option<u64> {
        self.driver.next_deadline_us()
    }

    /// Snapshot immediate work, wake subscriptions, and the next deadline.
    ///
    /// The command source is included only while the bounded driver can retain
    /// another request. A queued command makes the intent immediately runnable.
    pub fn wait_intent(&self) -> IncrementalWaitIntent {
        let intent = self.driver.wait_intent();
        let intent = if self.driver.can_submit() {
            intent.with_command(!self.state.shared.commands.is_empty())
        } else {
            intent
        };
        intent.with_cancellation(!self.state.shared.cancellations.is_empty())
    }

    /// Wait until the controller command channel is non-empty without consuming it.
    ///
    /// A platform adapter should keep at most one such future outstanding and
    /// only poll it when [`Self::wait_intent`] contains [`WaitSet::COMMAND`].
    pub async fn wait_for_command(&self) {
        self.state.shared.commands.ready_to_receive().await;
    }

    /// Wait for one subscribed source without consuming a controller command.
    ///
    /// Immediate internal work returns an empty set; pass the returned set to
    /// [`Self::run_once`] either way. Command readiness is registered through
    /// the facade's bounded channel, while `platform` owns backend, L2, and
    /// monotonic timer registration.
    pub async fn wait_ready<P: IncrementalWaitPlatform>(
        &self,
        platform: &mut P,
    ) -> Result<WaitSet, IncrementalWaitError<P::Error>> {
        let mut diagnostics = self.diagnostics.get();
        IncrementalRunnerDiagnostics::increment(&mut diagnostics.wait_ready_calls);
        self.diagnostics.set(diagnostics);

        let result = poll_fn(|cx| {
            let intent = self.wait_intent();
            if intent.run_immediately() {
                if !self.state.shared.cancellations.is_empty() {
                    return Poll::Ready(Ok(WaitSet::CANCEL));
                }
                return Poll::Ready(Ok(WaitSet::empty()));
            }

            let subscribed = intent.sources();
            let mut ready = WaitSet::empty();
            if subscribed.contains(WaitSet::COMMAND)
                && self
                    .state
                    .shared
                    .commands
                    .poll_ready_to_receive(cx)
                    .is_ready()
            {
                ready = ready.union(WaitSet::COMMAND);
            }
            if subscribed.contains(WaitSet::CANCEL)
                && self
                    .state
                    .shared
                    .cancellations
                    .poll_ready_to_receive(cx)
                    .is_ready()
            {
                ready = ready.union(WaitSet::CANCEL);
            }

            let platform_sources = subscribed
                .without(WaitSet::COMMAND)
                .without(WaitSet::CANCEL);
            if !platform_sources.is_empty() {
                match platform.poll_ready(cx, platform_sources, intent.deadline_us()) {
                    Poll::Pending => {}
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(IncrementalWaitError::Platform(error)));
                    }
                    Poll::Ready(Ok(platform_ready)) => {
                        if !platform_ready.without(platform_sources).is_empty() {
                            return Poll::Ready(Err(IncrementalWaitError::UnexpectedSources {
                                subscribed: platform_sources,
                                ready: platform_ready,
                            }));
                        }
                        ready = ready.union(platform_ready);
                    }
                }
            }

            if ready.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(ready))
            }
        })
        .await;

        let mut diagnostics = self.diagnostics.get();
        match result {
            Ok(ready) => {
                IncrementalRunnerDiagnostics::increment(&mut diagnostics.wait_ready_completions);
                if ready.is_empty() {
                    IncrementalRunnerDiagnostics::increment(
                        &mut diagnostics.immediate_ready_completions,
                    );
                }
                diagnostics.record_ready(ready);
            }
            Err(_) => {
                IncrementalRunnerDiagnostics::increment(&mut diagnostics.wait_ready_errors);
            }
        }
        self.diagnostics.set(diagnostics);
        result
    }

    /// Snapshot opt-in runner workload and wake-source counters.
    pub fn diagnostics(&self) -> IncrementalRunnerDiagnostics {
        self.diagnostics.get()
    }

    /// Borrow the chip backend for platform wake registration and diagnostics.
    pub const fn backend(&self) -> &B {
        self.driver.backend()
    }

    fn submit_command(&mut self, command: Command) -> Result<(), IncrementalRadioRunnerError> {
        let Some(sequence) = CommandSequence::try_from_raw(command.sequence) else {
            self.signal_protocol(command.sequence);
            return Err(IncrementalRadioRunnerError::InvalidCommandSequence);
        };
        let kind = CommandKindTag::from_command(&command.kind);
        if !self.ledger.insert(sequence, kind) {
            self.signal_protocol(command.sequence);
            return Err(IncrementalRadioRunnerError::CommandCapacity);
        }
        let request = match command.kind {
            CommandKind::Initialize => IncrementalRequest::Initialize(self.config),
            CommandKind::Scan(config) => IncrementalRequest::Scan(config),
            CommandKind::Connect(config) => IncrementalRequest::Connect(config),
            CommandKind::Disconnect => IncrementalRequest::Disconnect(self.config),
        };
        if let Err(error) = self.driver.submit(sequence, request) {
            let _ = self.ledger.remove(sequence);
            self.signal_protocol(command.sequence);
            return Err(Self::submit_error(error));
        }
        Ok(())
    }

    fn submit_error(error: SubmitError<IncrementalRequest>) -> IncrementalRadioRunnerError {
        IncrementalRadioRunnerError::CommandArbiter(error.reason())
    }

    fn publish_terminal(
        &mut self,
        event: IncrementalDriverEvent,
    ) -> Result<(), IncrementalRadioRunnerError> {
        match event {
            IncrementalDriverEvent::Completed {
                sequence,
                completion,
            } => {
                let Some(kind) = self.ledger.remove(sequence) else {
                    self.signal_protocol(sequence.get());
                    return Err(IncrementalRadioRunnerError::MissingCommand);
                };
                let Some((completion, wifi_event)) = Self::successful_completion(kind, completion)
                else {
                    self.signal_protocol(sequence.get());
                    return Err(IncrementalRadioRunnerError::CompletionMismatch);
                };
                if matches!(kind, CommandKindTag::Initialize)
                    && let Some(capabilities) = self.driver.backend().l2_capabilities()
                {
                    self.state.shared.l2_capabilities.publish_once(capabilities);
                }
                self.state.shared.publish_event(wifi_event);
                self.state.shared.completion.signal(Completion {
                    sequence: sequence.get(),
                    kind: completion,
                });
            }
            IncrementalDriverEvent::Cancelled { sequence, .. } => {
                let Some(kind) = self.ledger.remove(sequence) else {
                    self.signal_protocol(sequence.get());
                    return Err(IncrementalRadioRunnerError::MissingCommand);
                };
                let error = BackendError::new(BackendErrorClass::Cancelled, RUNNER_CANCELLED_CODE);
                self.publish_failed(sequence, kind, error);
            }
            IncrementalDriverEvent::Failed {
                sequence, error, ..
            } => {
                let Some(kind) = self.ledger.remove(sequence) else {
                    self.signal_protocol(sequence.get());
                    return Err(IncrementalRadioRunnerError::MissingCommand);
                };
                self.publish_failed(sequence, kind, error);
            }
            IncrementalDriverEvent::Idle
            | IncrementalDriverEvent::Started { .. }
            | IncrementalDriverEvent::CancelRequested { .. }
            | IncrementalDriverEvent::Waiting { .. }
            | IncrementalDriverEvent::Pending { .. }
            | IncrementalDriverEvent::BudgetExhausted { .. } => {}
        }
        Ok(())
    }

    fn successful_completion(
        kind: CommandKindTag,
        completion: IncrementalCompletion,
    ) -> Option<(CompletionKind, WifiEvent)> {
        match (kind, completion) {
            (CommandKindTag::Initialize, IncrementalCompletion::Initialized) => {
                Some((CompletionKind::Initialize(Ok(())), WifiEvent::Initialized))
            }
            (CommandKindTag::Scan, IncrementalCompletion::Scan(outcome)) => Some((
                CompletionKind::Scan(Ok(outcome)),
                WifiEvent::ScanCompleted {
                    count: outcome.count,
                    truncated: outcome.truncated,
                },
            )),
            (CommandKindTag::Connect, IncrementalCompletion::Connected(info)) => Some((
                CompletionKind::Connect(Ok(info)),
                WifiEvent::Connected(info),
            )),
            (CommandKindTag::Disconnect, IncrementalCompletion::Disconnected) => Some((
                CompletionKind::Disconnect(Ok(())),
                WifiEvent::Disconnected { reason: 0 },
            )),
            _ => None,
        }
    }

    fn publish_failed(&self, sequence: CommandSequence, kind: CommandKindTag, error: BackendError) {
        let completion = match kind {
            CommandKindTag::Initialize => CompletionKind::Initialize(Err(error)),
            CommandKindTag::Scan => CompletionKind::Scan(Err(error)),
            CommandKindTag::Connect => CompletionKind::Connect(Err(error)),
            CommandKindTag::Disconnect => CompletionKind::Disconnect(Err(error)),
        };
        self.state.shared.publish_event(WifiEvent::Failed(error));
        self.state.shared.completion.signal(Completion {
            sequence: sequence.get(),
            kind: completion,
        });
    }

    fn signal_protocol(&self, sequence: u32) {
        self.state.shared.completion.signal(Completion {
            sequence,
            kind: CompletionKind::Protocol,
        });
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::future::Future;
    use core::task::{Context, Poll, Waker};
    use std::boxed::Box;

    use super::*;
    use crate::{
        ConnectionInfo, Error, PollDisposition, RadioConfig, RadioResources, ScanConfig,
        ScanOutcome, ScanResult, Security, Ssid, WorkReport, init,
    };
    use crate::{OperationId, WakeReason};

    struct FakeBackend {
        completion: Option<IncrementalCompletion>,
        force_mismatch: bool,
        cancel_calls: u8,
        deadline_us: Option<u64>,
        pending_polls: u8,
    }

    impl FakeBackend {
        const fn new() -> Self {
            Self {
                completion: None,
                force_mismatch: false,
                cancel_calls: 0,
                deadline_us: None,
                pending_polls: 0,
            }
        }

        const fn mismatched() -> Self {
            Self {
                completion: None,
                force_mismatch: true,
                cancel_calls: 0,
                deadline_us: None,
                pending_polls: 0,
            }
        }

        const fn with_deadline(deadline_us: u64) -> Self {
            Self {
                completion: None,
                force_mismatch: false,
                cancel_calls: 0,
                deadline_us: Some(deadline_us),
                pending_polls: 1,
            }
        }

        const fn pending_once() -> Self {
            Self {
                completion: None,
                force_mismatch: false,
                cancel_calls: 0,
                deadline_us: None,
                pending_polls: 1,
            }
        }
    }

    impl IncrementalWifiBackend for FakeBackend {
        fn start(
            &mut self,
            _id: OperationId,
            request: IncrementalRequest,
        ) -> Result<(), BackendError> {
            self.completion = Some(if self.force_mismatch {
                IncrementalCompletion::Disconnected
            } else {
                match request {
                    IncrementalRequest::Initialize(_) => IncrementalCompletion::Initialized,
                    IncrementalRequest::Scan(_) => IncrementalCompletion::Scan(ScanOutcome {
                        count: 1,
                        truncated: false,
                    }),
                    IncrementalRequest::Connect(config) => {
                        IncrementalCompletion::Connected(ConnectionInfo {
                            bssid: config.bssid,
                            frequency_mhz: 2437,
                        })
                    }
                    IncrementalRequest::Disconnect(_) => IncrementalCompletion::Disconnected,
                }
            });
            Ok(())
        }

        fn poll(
            &mut self,
            id: OperationId,
            _reason: WakeReason,
            budget: WorkBudget,
            scan_output: &mut [ScanResult],
        ) -> Result<WorkReport, BackendError> {
            let completion = self.completion.expect("start precedes poll");
            if self.pending_polls != 0 {
                self.pending_polls -= 1;
                return Ok(WorkReport::try_new(
                    id,
                    budget,
                    1,
                    1,
                    true,
                    PollDisposition::Pending(WaitSet::BACKEND),
                )
                .unwrap());
            }
            if matches!(completion, IncrementalCompletion::Scan(_)) {
                scan_output[0] = ScanResult {
                    ssid: Ssid::try_from_bytes(b"test-ap").unwrap(),
                    bssid: [1, 2, 3, 4, 5, 6],
                    frequency_mhz: 2437,
                    rssi_dbm: -42,
                    security: Security::Wpa2Personal,
                    channel: 6,
                };
            }
            Ok(WorkReport::try_new(
                id,
                budget,
                1,
                1,
                true,
                PollDisposition::Complete(completion),
            )
            .unwrap())
        }

        fn cancel(&mut self, _id: OperationId) -> Result<(), BackendError> {
            self.cancel_calls += 1;
            Ok(())
        }

        fn next_deadline_us(&self, _id: OperationId) -> Option<u64> {
            self.deadline_us
        }

        fn l2_capabilities(&self) -> Option<crate::WifiL2Capabilities> {
            crate::WifiL2Capabilities::try_new([0x02, 6, 5, 4, 3, 2])
        }
    }

    #[derive(Default)]
    struct FakeWaitPlatform {
        ready: WaitSet,
        error: Option<u8>,
        calls: u8,
        last_sources: WaitSet,
        last_deadline_us: Option<u64>,
    }

    impl IncrementalWaitPlatform for FakeWaitPlatform {
        type Error = u8;

        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
            sources: WaitSet,
            deadline_us: Option<u64>,
        ) -> Poll<Result<WaitSet, Self::Error>> {
            self.calls += 1;
            self.last_sources = sources;
            self.last_deadline_us = deadline_us;
            if let Some(error) = self.error {
                return Poll::Ready(Err(error));
            }
            if self.ready.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(self.ready))
            }
        }
    }

    fn poll<F: Future>(future: core::pin::Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::noop();
        future.poll(&mut Context::from_waker(waker))
    }

    fn budget() -> WorkBudget {
        WorkBudget::try_new(4, 100).unwrap()
    }

    #[test]
    fn unified_wait_registers_command_and_platform_without_consuming() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::with_deadline(42),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());
        let mut platform = FakeWaitPlatform::default();
        let mut initialize = core::pin::pin!(wifi.controller.initialize());
        assert_eq!(wifi.device.l2_capabilities(), None);

        {
            let mut wait = core::pin::pin!(runner.wait_ready(&mut platform));
            assert!(poll(wait.as_mut()).is_pending());
            assert!(poll(initialize.as_mut()).is_pending());
            assert_eq!(poll(wait.as_mut()), Poll::Ready(Ok(WaitSet::empty())));
        }
        assert_eq!(platform.calls, 0, "idle runner waits only for commands");

        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Pending { .. }
        ));

        platform.ready = WaitSet::TIMER;
        {
            let mut wait = core::pin::pin!(runner.wait_ready(&mut platform));
            assert_eq!(poll(wait.as_mut()), Poll::Ready(Ok(WaitSet::TIMER)));
        }
        assert_eq!(
            platform.last_sources,
            WaitSet::BACKEND.union(WaitSet::TIMER)
        );
        assert_eq!(platform.last_deadline_us, Some(42));
        assert!(matches!(
            runner.run_once(WaitSet::TIMER).unwrap(),
            IncrementalDriverEvent::Completed { .. }
        ));
        assert_eq!(poll(initialize.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(
            wifi.device.station_mac_address(),
            Some([0x02, 6, 5, 4, 3, 2])
        );
        assert_eq!(
            runner.diagnostics(),
            IncrementalRunnerDiagnostics {
                run_once_calls: 3,
                timer_ready_batches: 2,
                wait_ready_calls: 2,
                wait_ready_completions: 2,
                immediate_ready_completions: 1,
                operations_started: 1,
                pending_polls: 1,
                operations_completed: 1,
                ..IncrementalRunnerDiagnostics::EMPTY
            }
        );
    }

    #[test]
    fn unified_wait_fails_closed_on_platform_error_or_unsubscribed_source() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::pending_once(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());
        let mut initialize = core::pin::pin!(wifi.controller.initialize());
        assert!(poll(initialize.as_mut()).is_pending());
        let _ = runner.run_once(WaitSet::empty()).unwrap();
        let _ = runner.run_once(WaitSet::empty()).unwrap();

        let mut failed = FakeWaitPlatform {
            error: Some(7),
            ..Default::default()
        };
        {
            let mut wait = core::pin::pin!(runner.wait_ready(&mut failed));
            assert_eq!(
                poll(wait.as_mut()),
                Poll::Ready(Err(IncrementalWaitError::Platform(7)))
            );
        }

        let mut invalid = FakeWaitPlatform {
            ready: WaitSet::L2_RX,
            ..Default::default()
        };
        {
            let mut wait = core::pin::pin!(runner.wait_ready(&mut invalid));
            assert_eq!(
                poll(wait.as_mut()),
                Poll::Ready(Err(IncrementalWaitError::UnexpectedSources {
                    subscribed: WaitSet::BACKEND,
                    ready: WaitSet::L2_RX,
                }))
            );
        }
        assert_eq!(runner.diagnostics().wait_ready_calls, 2);
        assert_eq!(runner.diagnostics().wait_ready_errors, 2);
        assert_eq!(runner.diagnostics().wait_ready_completions, 0);
    }

    #[test]
    fn incremental_diagnostic_counters_saturate() {
        let mut diagnostics = IncrementalRunnerDiagnostics::EMPTY;
        diagnostics.run_once_calls = u32::MAX;
        diagnostics.command_ready_batches = u32::MAX;
        IncrementalRunnerDiagnostics::increment(&mut diagnostics.run_once_calls);
        diagnostics.record_ready(WaitSet::COMMAND);
        assert_eq!(diagnostics.run_once_calls, u32::MAX);
        assert_eq!(diagnostics.command_ready_batches, u32::MAX);
    }

    #[test]
    fn incremental_split_preserves_controller_and_scan_contracts() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::new(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());

        let idle = runner.wait_intent();
        assert_eq!(idle.sources(), WaitSet::COMMAND.union(WaitSet::CANCEL));
        assert_eq!(idle.deadline_us(), None);
        assert!(!idle.run_immediately());

        {
            let mut initialize = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(initialize.as_mut()).is_pending());
            assert!(runner.wait_intent().run_immediately());
            assert!(matches!(
                runner.run_once(WaitSet::empty()).unwrap(),
                IncrementalDriverEvent::Started { .. }
            ));
            assert_eq!(
                runner.wait_intent().sources(),
                WaitSet::COMMAND.union(WaitSet::CANCEL)
            );
            assert!(runner.wait_intent().run_immediately());
            assert!(matches!(
                runner.run_once(WaitSet::empty()).unwrap(),
                IncrementalDriverEvent::Completed {
                    completion: IncrementalCompletion::Initialized,
                    ..
                }
            ));
            assert_eq!(poll(initialize.as_mut()), Poll::Ready(Ok(())));
        }

        let mut results = [ScanResult::empty(); 1];
        {
            let mut scan = core::pin::pin!(wifi.controller.scan(
                ScanConfig::new(crate::OperationTimeout::try_from_millis(1_000).unwrap()),
                &mut results,
            ));
            assert!(poll(scan.as_mut()).is_pending());
            let _ = runner.run_once(WaitSet::empty()).unwrap();
            let _ = runner.run_once(WaitSet::empty()).unwrap();
            assert_eq!(
                poll(scan.as_mut()),
                Poll::Ready(Ok(ScanOutcome {
                    count: 1,
                    truncated: false,
                }))
            );
        }
        assert_eq!(results[0].ssid.as_bytes(), b"test-ap");
    }

    #[test]
    fn dropped_future_cancels_before_starting_replacement() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::new(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());

        {
            let mut abandoned = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(abandoned.as_mut()).is_pending());
            let _ = runner.run_once(WaitSet::empty()).unwrap();
        }

        let mut disconnect = core::pin::pin!(wifi.controller.disconnect());
        assert!(poll(disconnect.as_mut()).is_pending());
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::CancelRequested { .. }
        ));
        assert_eq!(runner.backend().cancel_calls, 1);
        assert!(matches!(
            runner.run_once(WaitSet::BACKEND).unwrap(),
            IncrementalDriverEvent::Cancelled {
                suppressed_completion: true,
                ..
            }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        let _ = runner.run_once(WaitSet::BACKEND).unwrap();
        assert_eq!(poll(disconnect.as_mut()), Poll::Ready(Ok(())));
    }

    #[test]
    fn dropped_future_requests_cancellation_without_a_replacement() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::new(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());

        {
            let mut abandoned = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(abandoned.as_mut()).is_pending());
            assert!(matches!(
                runner.run_once(WaitSet::empty()).unwrap(),
                IncrementalDriverEvent::Started { .. }
            ));
        }

        assert!(runner.wait_intent().run_immediately());
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::CancelRequested { .. }
        ));
        assert_eq!(runner.backend().cancel_calls, 1);
        assert!(matches!(
            runner.run_once(WaitSet::BACKEND).unwrap(),
            IncrementalDriverEvent::Cancelled {
                suppressed_completion: true,
                ..
            }
        ));
    }

    #[test]
    fn dropped_future_wakes_the_unified_wait_for_cancellation() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::pending_once(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts { mut wifi, runner } = radio.split_incremental(budget());
        let mut runner = runner;
        let mut initialize = Box::pin(wifi.controller.initialize());
        assert!(poll(initialize.as_mut()).is_pending());
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Pending { .. }
        ));

        let mut platform = FakeWaitPlatform::default();
        let mut wait = Box::pin(runner.wait_ready(&mut platform));
        assert!(poll(wait.as_mut()).is_pending());
        drop(initialize);
        assert_eq!(poll(wait.as_mut()), Poll::Ready(Ok(WaitSet::CANCEL)));
    }

    #[test]
    fn queued_replacement_waits_for_pending_capacity() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::new(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());

        {
            let mut first = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(first.as_mut()).is_pending());
            let _ = runner.run_once(WaitSet::empty()).unwrap();
        }
        let mut scan_results = [ScanResult::empty(); 1];
        {
            let mut second = core::pin::pin!(wifi.controller.scan(
                ScanConfig::new(crate::OperationTimeout::try_from_millis(1_000).unwrap()),
                &mut scan_results,
            ));
            assert!(poll(second.as_mut()).is_pending());
            assert!(matches!(
                runner.run_once(WaitSet::empty()).unwrap(),
                IncrementalDriverEvent::CancelRequested { .. }
            ));
        }

        let mut third = core::pin::pin!(wifi.controller.disconnect());
        assert!(poll(third.as_mut()).is_pending());

        let backpressured = runner.wait_intent();
        assert_eq!(backpressured.sources(), WaitSet::CANCEL);
        assert!(!backpressured.sources().contains(WaitSet::COMMAND));
        assert!(backpressured.run_immediately());

        // The third command remains in the facade channel until the pending
        // second command has started; it is not rejected as over-capacity.
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Cancelled { .. }
        ));
        assert!(runner.wait_intent().run_immediately());
        assert!(runner.wait_intent().sources().contains(WaitSet::COMMAND));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Cancelled { .. }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        assert!(runner.wait_intent().sources().contains(WaitSet::COMMAND));
        assert!(runner.wait_intent().run_immediately());
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Completed {
                completion: IncrementalCompletion::Disconnected,
                ..
            }
        ));
        assert_eq!(poll(third.as_mut()), Poll::Ready(Ok(())));
    }

    #[test]
    fn mismatched_completion_unblocks_controller_with_protocol_error() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: FakeBackend::mismatched(),
                device: (),
            },
            state,
        )
        .unwrap();
        let IncrementalRadioParts {
            mut wifi,
            mut runner,
        } = radio.split_incremental(budget());
        let mut initialize = core::pin::pin!(wifi.controller.initialize());
        assert!(poll(initialize.as_mut()).is_pending());
        let _ = runner.run_once(WaitSet::empty()).unwrap();
        assert_eq!(
            runner.run_once(WaitSet::BACKEND),
            Err(IncrementalRadioRunnerError::CompletionMismatch)
        );
        assert_eq!(poll(initialize.as_mut()), Poll::Ready(Err(Error::Protocol)));
    }
}
