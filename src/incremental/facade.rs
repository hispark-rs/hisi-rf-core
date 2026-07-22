use crate::wifi::{Command, CommandKind, Completion, CompletionKind};
use crate::{
    BackendError, BackendErrorClass, RadioController, RadioState, WifiConfig, WifiEvent, WifiParts,
};

use super::{
    CommandArbiterError, CommandSequence, IncrementalBackendDriver, IncrementalCompletion,
    IncrementalDriverError, IncrementalDriverEvent, IncrementalRequest, IncrementalWaitIntent,
    IncrementalWifiBackend, SubmitError, WaitSet, WorkBudget,
};

// A runner-local cancellation has no chip/backend status code to preserve.
const RUNNER_CANCELLED_CODE: u32 = 0;

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
}

impl<B: IncrementalWifiBackend, const EVENTS: usize> IncrementalRadioRunner<B, EVENTS> {
    /// Accept at most one command and execute at most one bounded driver action.
    pub fn run_once(
        &mut self,
        ready: WaitSet,
    ) -> Result<IncrementalDriverEvent, IncrementalRadioRunnerError> {
        if self.driver.can_submit() {
            if let Ok(command) = self.state.shared.commands.try_receive() {
                self.submit_command(command)?;
            }
        }

        // SAFETY: this unique runner is the only writer. A terminal scan
        // completion is signalled only after `drive_once` returns and releases
        // the mutable borrow, matching the blocking runner's ownership rule.
        let scan_output = unsafe { &mut *self.state.shared.scan_results_ptr() };
        let event = self.driver.drive_once(ready, scan_output)?;
        self.publish_terminal(event)?;
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
        if self.driver.can_submit() {
            intent.with_command(!self.state.shared.commands.is_empty())
        } else {
            intent
        }
    }

    /// Wait until the controller command channel is non-empty without consuming it.
    ///
    /// A platform adapter should keep at most one such future outstanding and
    /// only poll it when [`Self::wait_intent`] contains [`WaitSet::COMMAND`].
    pub async fn wait_for_command(&self) {
        self.state.shared.commands.ready_to_receive().await;
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
    }

    impl FakeBackend {
        const fn new() -> Self {
            Self {
                completion: None,
                force_mismatch: false,
                cancel_calls: 0,
            }
        }

        const fn mismatched() -> Self {
            Self {
                completion: None,
                force_mismatch: true,
                cancel_calls: 0,
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
            None
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
        assert_eq!(idle.sources(), WaitSet::COMMAND);
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
                WaitSet::BACKEND.union(WaitSet::COMMAND)
            );
            assert!(matches!(
                runner.run_once(WaitSet::BACKEND).unwrap(),
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
                ScanConfig::try_from_timeout_ms(1_000).unwrap(),
                &mut results,
            ));
            assert!(poll(scan.as_mut()).is_pending());
            let _ = runner.run_once(WaitSet::empty()).unwrap();
            let _ = runner.run_once(WaitSet::BACKEND).unwrap();
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
                ScanConfig::try_from_timeout_ms(1_000).unwrap(),
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
        assert_eq!(backpressured.sources(), WaitSet::BACKEND);
        assert!(!backpressured.sources().contains(WaitSet::COMMAND));
        assert!(!backpressured.run_immediately());

        // The third command remains in the facade channel until the pending
        // second command has started; it is not rejected as over-capacity.
        assert!(matches!(
            runner.run_once(WaitSet::BACKEND).unwrap(),
            IncrementalDriverEvent::Cancelled { .. }
        ));
        assert!(runner.wait_intent().run_immediately());
        assert!(!runner.wait_intent().sources().contains(WaitSet::COMMAND));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        assert!(runner.wait_intent().sources().contains(WaitSet::COMMAND));
        assert!(runner.wait_intent().run_immediately());
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::CancelRequested { .. }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::BACKEND).unwrap(),
            IncrementalDriverEvent::Cancelled { .. }
        ));
        assert!(matches!(
            runner.run_once(WaitSet::empty()).unwrap(),
            IncrementalDriverEvent::Started { .. }
        ));
        let _ = runner.run_once(WaitSet::BACKEND).unwrap();
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
