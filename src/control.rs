//! Internal bounded controller-to-runner command transport.
//!
//! This module is public only so independently released facade and chip crates
//! can share one implementation. Applications should use `hisi-rf` protocol
//! handles instead of naming these transport types.

use core::{cell::RefCell, num::NonZeroU32};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};

/// Conservation snapshot for one bounded unsolicited-event queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventQueueDiagnostics {
    /// Events accepted from the runner.
    pub accepted: u32,
    /// Events consumed by the protocol handle.
    pub consumed: u32,
    /// Events rejected because the queue was full.
    pub dropped: u32,
    /// Events currently waiting for the protocol handle.
    pub pending: usize,
    /// Largest observed queue occupancy.
    pub high_water: usize,
}

/// An unsolicited event could not enter the bounded queue.
#[derive(Debug)]
pub struct EventPublishError<E> {
    event: E,
}

impl<E> EventPublishError<E> {
    /// Recover the event rejected by backpressure.
    pub fn into_inner(self) -> E {
        self.event
    }
}

/// Caller-owned state for unsolicited protocol events.
///
/// This queue is deliberately separate from [`ControlState`]: consuming an
/// event can never consume or overwrite a command completion.
pub struct EventState<E, const CAPACITY: usize> {
    claimed: AtomicBool,
    events: Channel<CriticalSectionRawMutex, E, CAPACITY>,
    accepted: AtomicU32,
    consumed: AtomicU32,
    dropped: AtomicU32,
    high_water: AtomicU32,
}

impl<E, const CAPACITY: usize> EventState<E, CAPACITY> {
    /// Construct unclaimed event storage.
    pub const fn new() -> Self {
        assert!(CAPACITY > 0, "protocol event queue must not be empty");
        Self {
            claimed: AtomicBool::new(false),
            events: Channel::new(),
            accepted: AtomicU32::new(0),
            consumed: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            high_water: AtomicU32::new(0),
        }
    }

    /// Split the state into unique runner and protocol capabilities.
    pub fn claim(
        &'static self,
    ) -> Option<(EventProducer<E, CAPACITY>, EventConsumer<E, CAPACITY>)> {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some((EventProducer { state: self }, EventConsumer { state: self }))
    }

    fn diagnostics(&self) -> EventQueueDiagnostics {
        EventQueueDiagnostics {
            accepted: self.accepted.load(Ordering::Relaxed),
            consumed: self.consumed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            pending: self.events.len(),
            high_water: self.high_water.load(Ordering::Relaxed) as usize,
        }
    }
}

impl<E, const CAPACITY: usize> Default for EventState<E, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique runner-side capability for publishing unsolicited events.
pub struct EventProducer<E: 'static, const CAPACITY: usize> {
    state: &'static EventState<E, CAPACITY>,
}

impl<E, const CAPACITY: usize> EventProducer<E, CAPACITY> {
    /// Publish one event without blocking the radio runner.
    pub fn try_publish(&mut self, event: E) -> Result<(), EventPublishError<E>> {
        match self.state.events.try_send(event) {
            Ok(()) => {
                self.state.accepted.fetch_add(1, Ordering::Relaxed);
                self.state
                    .high_water
                    .fetch_max(self.state.events.len() as u32, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(event)) => {
                self.state.dropped.fetch_add(1, Ordering::Relaxed);
                Err(EventPublishError { event })
            }
        }
    }

    /// Snapshot event conservation counters.
    pub fn diagnostics(&self) -> EventQueueDiagnostics {
        self.state.diagnostics()
    }
}

/// Unique protocol-side capability for receiving unsolicited events.
pub struct EventConsumer<E: 'static, const CAPACITY: usize> {
    state: &'static EventState<E, CAPACITY>,
}

impl<E, const CAPACITY: usize> EventConsumer<E, CAPACITY> {
    /// Take the oldest event without waiting.
    pub fn try_next_event(&mut self) -> Option<E> {
        let event = self.state.events.try_receive().ok()?;
        self.state.consumed.fetch_add(1, Ordering::Relaxed);
        Some(event)
    }

    /// Wait for and take the oldest event.
    pub async fn next_event(&mut self) -> E {
        let event = self.state.events.receive().await;
        self.state.consumed.fetch_add(1, Ordering::Relaxed);
        event
    }

    /// Snapshot event conservation counters.
    pub fn diagnostics(&self) -> EventQueueDiagnostics {
        self.state.diagnostics()
    }
}

/// Generation-tagged identity for one accepted control command.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ControlId(NonZeroU32);

impl ControlId {
    /// Validate a raw identity received across an FFI or diagnostic boundary.
    pub const fn try_from_raw(raw: u32) -> Option<Self> {
        match NonZeroU32::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the stable non-zero representation.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// One command whose ownership moved from a controller to its runner.
#[derive(Debug)]
pub struct ControlCommand<C> {
    id: ControlId,
    command: C,
}

impl<C> ControlCommand<C> {
    /// Identity assigned by the unique controller.
    pub const fn id(&self) -> ControlId {
        self.id
    }

    /// Recover the owned protocol command.
    pub fn into_inner(self) -> C {
        self.command
    }
}

/// One terminal result published by the unique runner.
#[derive(Debug)]
pub struct ControlCompletion<R> {
    id: ControlId,
    result: R,
}

impl<R> ControlCompletion<R> {
    /// Identity of the completed command.
    pub const fn id(&self) -> ControlId {
        self.id
    }

    /// Recover the owned protocol result.
    pub fn into_inner(self) -> R {
        self.result
    }
}

/// Rejected submission that preserves command ownership.
#[derive(Debug)]
pub struct ControlSubmitError<C> {
    command: C,
}

impl<C> ControlSubmitError<C> {
    /// Recover the command that was not accepted.
    pub fn into_inner(self) -> C {
        self.command
    }
}

/// A completion did not belong to the controller's outstanding command.
#[derive(Debug)]
pub struct StaleControlCompletion<R> {
    expected: Option<ControlId>,
    received: ControlId,
    result: R,
}

impl<R> StaleControlCompletion<R> {
    /// Identity still owned by the controller.
    pub const fn expected(&self) -> Option<ControlId> {
        self.expected
    }

    /// Unexpected identity supplied by the transport.
    pub const fn received(&self) -> ControlId {
        self.received
    }

    /// Recover the unmatched result for diagnostics or fail-closed cleanup.
    pub fn into_inner(self) -> R {
        self.result
    }
}

/// Runner-side completion failure that preserves result ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlCompleteErrorKind {
    /// No command is currently owned by the runner.
    NoActiveCommand,
    /// The supplied identity does not match the runner-owned command.
    StaleCommand,
}

/// Rejected runner completion.
#[derive(Debug)]
pub struct ControlCompleteError<R> {
    kind: ControlCompleteErrorKind,
    result: R,
}

impl<R> ControlCompleteError<R> {
    /// Stable failure class.
    pub const fn kind(&self) -> ControlCompleteErrorKind {
        self.kind
    }

    /// Recover the result that was not published.
    pub fn into_inner(self) -> R {
        self.result
    }
}

/// Caller-owned state shared by one protocol controller and runner.
pub struct ControlState<C, R> {
    claimed: AtomicBool,
    commands: Channel<CriticalSectionRawMutex, ControlCommand<C>, 1>,
    completion: Signal<CriticalSectionRawMutex, ControlCompletion<R>>,
}

impl<C, R> ControlState<C, R> {
    /// Construct unclaimed process-lifetime storage.
    pub const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            commands: Channel::new(),
            completion: Signal::new(),
        }
    }

    /// Split the state into non-cloneable controller and runner capabilities.
    pub fn claim(&'static self) -> Option<(ControlSender<C, R>, ControlReceiver<C, R>)> {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some((
            ControlSender {
                state: self,
                next_id: 0,
                outstanding: None,
            },
            ControlReceiver {
                state: self,
                active: None,
            },
        ))
    }
}

impl<C, R> Default for ControlState<C, R> {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique protocol-side command capability.
pub struct ControlSender<C: 'static, R: 'static> {
    state: &'static ControlState<C, R>,
    next_id: u32,
    outstanding: Option<ControlId>,
}

impl<C, R> ControlSender<C, R> {
    /// Submit one command without blocking or replacing live work.
    pub fn try_submit(&mut self, command: C) -> Result<ControlId, ControlSubmitError<C>> {
        if self.outstanding.is_some() {
            return Err(ControlSubmitError { command });
        }
        let id = self.allocate_id();
        match self.state.commands.try_send(ControlCommand { id, command }) {
            Ok(()) => {
                self.outstanding = Some(id);
                Ok(id)
            }
            Err(TrySendError::Full(command)) => Err(ControlSubmitError {
                command: command.command,
            }),
        }
    }

    /// Take the matching terminal completion, if the runner published one.
    pub fn try_take_completion(
        &mut self,
    ) -> Result<Option<ControlCompletion<R>>, StaleControlCompletion<R>> {
        let Some(completion) = self.state.completion.try_take() else {
            return Ok(None);
        };
        let Some(expected) = self.outstanding else {
            return Err(StaleControlCompletion {
                expected: None,
                received: completion.id,
                result: completion.result,
            });
        };
        if completion.id != expected {
            return Err(StaleControlCompletion {
                expected: Some(expected),
                received: completion.id,
                result: completion.result,
            });
        }
        self.outstanding = None;
        Ok(Some(completion))
    }

    /// Identity currently owned by the controller.
    pub const fn outstanding(&self) -> Option<ControlId> {
        self.outstanding
    }

    fn allocate_id(&mut self) -> ControlId {
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        ControlId(NonZeroU32::new(self.next_id).expect("control id is non-zero"))
    }
}

/// Unique runner-side command capability.
pub struct ControlReceiver<C: 'static, R: 'static> {
    state: &'static ControlState<C, R>,
    active: Option<ControlId>,
}

impl<C, R> ControlReceiver<C, R> {
    /// Take the oldest command when the runner does not already own one.
    pub fn try_take_command(&mut self) -> Option<ControlCommand<C>> {
        if self.active.is_some() {
            return None;
        }
        let command = self.state.commands.try_receive().ok()?;
        self.active = Some(command.id);
        Some(command)
    }

    /// Publish exactly one terminal result for the runner-owned command.
    pub fn complete(&mut self, id: ControlId, result: R) -> Result<(), ControlCompleteError<R>> {
        let Some(active) = self.active else {
            return Err(ControlCompleteError {
                kind: ControlCompleteErrorKind::NoActiveCommand,
                result,
            });
        };
        if active != id {
            return Err(ControlCompleteError {
                kind: ControlCompleteErrorKind::StaleCommand,
                result,
            });
        }
        self.state
            .completion
            .signal(ControlCompletion { id, result });
        self.active = None;
        Ok(())
    }

    /// Identity currently owned by the runner.
    pub const fn active(&self) -> Option<ControlId> {
        self.active
    }
}

/// Generation-tagged identity for one active protocol lifecycle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LifecycleId(NonZeroU32);

impl LifecycleId {
    /// Return the stable non-zero representation.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecyclePhase {
    Idle,
    Starting(LifecycleId),
    Active(LifecycleId),
    Cancelling {
        id: LifecycleId,
        waiter_attached: bool,
    },
    Terminal(LifecycleId),
}

struct LifecycleInner {
    next_generation: u32,
    phase: LifecyclePhase,
}

/// A lifecycle cannot begin or transition from its current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleStateError {
    /// Another generation still owns the lifecycle.
    Busy,
    /// The supplied handle belongs to an older or unrelated generation.
    Stale,
    /// The requested transition is invalid for the current phase.
    InvalidTransition,
}

struct LifecycleTerminal<E> {
    id: LifecycleId,
    result: Result<(), E>,
}

/// Error returned by an explicit lifecycle stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleStopError<E> {
    /// The guard no longer names the current lifecycle generation.
    Stale,
    /// The backend rejected or failed the stop operation.
    Backend(E),
}

/// Caller-owned state for one generation-tagged active lifecycle.
///
/// The state has one runner owner and at most one active guard. Dropping the
/// guard submits a nonblocking best-effort cancellation. Calling
/// [`LifecycleGuard::stop`] keeps a waiter attached until the runner publishes
/// the terminal backend result.
pub struct LifecycleState<E> {
    claimed: AtomicBool,
    inner: Mutex<CriticalSectionRawMutex, RefCell<LifecycleInner>>,
    cancellation: Signal<CriticalSectionRawMutex, LifecycleId>,
    terminal: Signal<CriticalSectionRawMutex, LifecycleTerminal<E>>,
}

impl<E> LifecycleState<E> {
    /// Construct unclaimed lifecycle storage.
    pub const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            inner: Mutex::new(RefCell::new(LifecycleInner {
                next_generation: 0,
                phase: LifecyclePhase::Idle,
            })),
            cancellation: Signal::new(),
            terminal: Signal::new(),
        }
    }

    /// Claim the unique runner capability.
    pub fn claim(&'static self) -> Option<LifecycleRunner<E>> {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(LifecycleRunner { state: self })
    }

    fn request_cancel(
        &self,
        id: LifecycleId,
        waiter_attached: bool,
    ) -> Result<(), LifecycleStateError> {
        let outcome = self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Starting(current) | LifecyclePhase::Active(current)
                    if current == id =>
                {
                    inner.phase = LifecyclePhase::Cancelling {
                        id,
                        waiter_attached,
                    };
                    Ok(true)
                }
                LifecyclePhase::Cancelling { id: current, .. } if current == id => Ok(false),
                LifecyclePhase::Terminal(current) if current == id => Ok(false),
                LifecyclePhase::Idle
                | LifecyclePhase::Starting(_)
                | LifecyclePhase::Active(_)
                | LifecyclePhase::Cancelling { .. }
                | LifecyclePhase::Terminal(_) => Err(LifecycleStateError::Stale),
            }
        })?;
        if outcome {
            self.cancellation.signal(id);
        }
        Ok(())
    }

    fn detach_waiter(&self, id: LifecycleId) {
        self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Cancelling {
                    id: current,
                    waiter_attached: true,
                } if current == id => {
                    inner.phase = LifecyclePhase::Cancelling {
                        id,
                        waiter_attached: false,
                    };
                }
                LifecyclePhase::Terminal(current) if current == id => {
                    let _ = self.terminal.try_take();
                    inner.phase = LifecyclePhase::Idle;
                }
                _ => {}
            }
        });
    }

    fn acknowledge_terminal(&self, id: LifecycleId) -> Result<(), LifecycleStateError> {
        self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Terminal(current) if current == id => {
                    inner.phase = LifecyclePhase::Idle;
                    Ok(())
                }
                _ => Err(LifecycleStateError::Stale),
            }
        })
    }
}

impl<E> Default for LifecycleState<E> {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique runner-side lifecycle state-machine capability.
pub struct LifecycleRunner<E: 'static> {
    state: &'static LifecycleState<E>,
}

impl<E> LifecycleRunner<E> {
    /// Reserve a fresh generation before submitting a backend start request.
    pub fn begin(&mut self) -> Result<LifecycleId, LifecycleStateError> {
        self.state.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            if inner.phase != LifecyclePhase::Idle {
                return Err(LifecycleStateError::Busy);
            }
            inner.next_generation = inner.next_generation.wrapping_add(1);
            if inner.next_generation == 0 {
                inner.next_generation = 1;
            }
            let id = LifecycleId(
                NonZeroU32::new(inner.next_generation).expect("generation is non-zero"),
            );
            inner.phase = LifecyclePhase::Starting(id);
            Ok(id)
        })
    }

    /// Abort a start request that the backend rejected synchronously.
    pub fn abort_start(&mut self, id: LifecycleId) -> Result<(), LifecycleStateError> {
        self.state.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Starting(current) if current == id => {
                    inner.phase = LifecyclePhase::Idle;
                    Ok(())
                }
                _ => Err(LifecycleStateError::Stale),
            }
        })
    }

    /// Convert the matching backend start callback into the unique active guard.
    pub fn activate(&mut self, id: LifecycleId) -> Result<LifecycleGuard<E>, LifecycleStateError> {
        self.state.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Starting(current) if current == id => {
                    inner.phase = LifecyclePhase::Active(id);
                    Ok(LifecycleGuard {
                        state: self.state,
                        id,
                        cancel_requested: false,
                        terminal_consumed: false,
                    })
                }
                _ => Err(LifecycleStateError::Stale),
            }
        })
    }

    /// Take one cancellation request without waiting.
    pub fn try_take_cancel(&mut self) -> Option<LifecycleId> {
        self.state.cancellation.try_take()
    }

    /// Publish the terminal result of the matching backend stop operation.
    pub fn finish(
        &mut self,
        id: LifecycleId,
        result: Result<(), E>,
    ) -> Result<(), LifecycleStateError> {
        let waiter_attached = self.state.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            match inner.phase {
                LifecyclePhase::Cancelling {
                    id: current,
                    waiter_attached,
                } if current == id => {
                    inner.phase = if waiter_attached {
                        LifecyclePhase::Terminal(id)
                    } else {
                        LifecyclePhase::Idle
                    };
                    Ok(waiter_attached)
                }
                _ => Err(LifecycleStateError::Stale),
            }
        })?;
        if waiter_attached {
            self.state.terminal.signal(LifecycleTerminal { id, result });
        }
        Ok(())
    }
}

/// Unique active lifecycle token.
#[must_use = "dropping an active lifecycle guard requests best-effort cancellation"]
pub struct LifecycleGuard<E: 'static> {
    state: &'static LifecycleState<E>,
    id: LifecycleId,
    cancel_requested: bool,
    terminal_consumed: bool,
}

impl<E> LifecycleGuard<E> {
    /// Identity shared by start, cancellation, and terminal observations.
    pub const fn id(&self) -> LifecycleId {
        self.id
    }

    /// Request cancellation and wait for the runner's backend result.
    pub async fn stop(mut self) -> Result<(), LifecycleStopError<E>> {
        if self.state.request_cancel(self.id, true).is_err() {
            self.terminal_consumed = true;
            return Err(LifecycleStopError::Stale);
        }
        self.cancel_requested = true;
        let terminal = self.state.terminal.wait().await;
        if terminal.id != self.id || self.state.acknowledge_terminal(self.id).is_err() {
            self.terminal_consumed = true;
            return Err(LifecycleStopError::Stale);
        }
        self.terminal_consumed = true;
        terminal.result.map_err(LifecycleStopError::Backend)
    }
}

impl<E> Drop for LifecycleGuard<E> {
    fn drop(&mut self) {
        if self.terminal_consumed {
            return;
        }
        if self.cancel_requested {
            self.state.detach_waiter(self.id);
        } else {
            let _ = self.state.request_cancel(self.id, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::task::{Context, Poll, Waker};

    fn state() -> &'static ControlState<u32, u32> {
        Box::leak(Box::new(ControlState::new()))
    }

    #[test]
    fn state_can_only_be_claimed_once() {
        let state = state();
        assert!(state.claim().is_some());
        assert!(state.claim().is_none());
    }

    #[test]
    fn command_and_completion_have_single_owners() {
        let (mut sender, mut receiver) = state().claim().unwrap();
        let id = sender.try_submit(7).unwrap();
        assert_eq!(sender.outstanding(), Some(id));

        let rejected = sender.try_submit(8).unwrap_err();
        assert_eq!(rejected.into_inner(), 8);

        let command = receiver.try_take_command().unwrap();
        assert_eq!(command.id(), id);
        assert_eq!(command.into_inner(), 7);
        assert_eq!(receiver.active(), Some(id));
        assert!(receiver.try_take_command().is_none());

        receiver.complete(id, 9).unwrap();
        assert_eq!(receiver.active(), None);
        let completion = sender.try_take_completion().unwrap().unwrap();
        assert_eq!(completion.id(), id);
        assert_eq!(completion.into_inner(), 9);
        assert_eq!(sender.outstanding(), None);
    }

    #[test]
    fn stale_and_duplicate_completions_fail_closed() {
        let (mut sender, mut receiver) = state().claim().unwrap();
        let id = sender.try_submit(1).unwrap();
        let _ = receiver.try_take_command().unwrap();
        let stale = ControlId::try_from_raw(id.get().wrapping_add(1)).unwrap();
        let error = receiver.complete(stale, 2).unwrap_err();
        assert_eq!(error.kind(), ControlCompleteErrorKind::StaleCommand);
        assert_eq!(error.into_inner(), 2);
        assert_eq!(receiver.active(), Some(id));

        receiver.complete(id, 3).unwrap();
        let error = receiver.complete(id, 4).unwrap_err();
        assert_eq!(error.kind(), ControlCompleteErrorKind::NoActiveCommand);
        assert_eq!(error.into_inner(), 4);
        assert_eq!(
            sender.try_take_completion().unwrap().unwrap().into_inner(),
            3
        );
    }

    #[test]
    fn unsolicited_completion_is_not_accepted() {
        let state = state();
        let (mut sender, _receiver) = state.claim().unwrap();
        let id = ControlId::try_from_raw(1).unwrap();
        state.completion.signal(ControlCompletion { id, result: 5 });

        let error = sender.try_take_completion().unwrap_err();
        assert_eq!(error.expected(), None);
        assert_eq!(error.received(), id);
        assert_eq!(error.into_inner(), 5);
        assert_eq!(sender.outstanding(), None);
    }

    #[test]
    fn event_queue_is_separate_bounded_and_conservative() {
        let state = Box::leak(Box::new(EventState::<u32, 2>::new()));
        let (mut producer, mut consumer) = state.claim().unwrap();
        assert!(state.claim().is_none());

        producer.try_publish(10).unwrap();
        producer.try_publish(20).unwrap();
        assert_eq!(producer.try_publish(30).unwrap_err().into_inner(), 30);
        assert_eq!(
            producer.diagnostics(),
            EventQueueDiagnostics {
                accepted: 2,
                consumed: 0,
                dropped: 1,
                pending: 2,
                high_water: 2,
            }
        );

        assert_eq!(consumer.try_next_event(), Some(10));
        assert_eq!(consumer.try_next_event(), Some(20));
        assert_eq!(consumer.try_next_event(), None);
        let diagnostics = consumer.diagnostics();
        assert_eq!(diagnostics.accepted, diagnostics.consumed);
        assert_eq!(diagnostics.pending, 0);
        assert_eq!(diagnostics.dropped, 1);
    }

    #[test]
    fn async_event_wait_is_woken_by_the_producer() {
        let state = Box::leak(Box::new(EventState::<u32, 1>::new()));
        let (mut producer, mut consumer) = state.claim().unwrap();
        let mut future = core::pin::pin!(consumer.next_event());
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        producer.try_publish(42).unwrap();
        assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(42));
    }

    #[test]
    fn explicit_lifecycle_stop_waits_for_matching_terminal_result() {
        let state = Box::leak(Box::new(LifecycleState::<u32>::new()));
        let mut runner = state.claim().unwrap();
        assert!(state.claim().is_none());
        let id = runner.begin().unwrap();
        let guard = runner.activate(id).unwrap();
        let mut stop = Box::pin(guard.stop());
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(stop.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(runner.try_take_cancel(), Some(id));
        runner.finish(id, Ok(())).unwrap();
        assert_eq!(stop.as_mut().poll(&mut context), Poll::Ready(Ok(())));
        assert_ne!(runner.begin().unwrap(), id);
    }

    #[test]
    fn dropped_lifecycle_guard_requests_detached_best_effort_cancel() {
        let state = Box::leak(Box::new(LifecycleState::<u32>::new()));
        let mut runner = state.claim().unwrap();
        let id = runner.begin().unwrap();
        drop(runner.activate(id).unwrap());
        assert_eq!(runner.try_take_cancel(), Some(id));
        runner.finish(id, Err(7)).unwrap();
        assert!(runner.begin().is_ok());
    }

    #[test]
    fn dropping_an_inflight_stop_waiter_does_not_strand_the_lifecycle() {
        let state = Box::leak(Box::new(LifecycleState::<u32>::new()));
        let mut runner = state.claim().unwrap();
        let id = runner.begin().unwrap();
        let guard = runner.activate(id).unwrap();
        let mut stop = Box::pin(guard.stop());
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(stop.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(runner.try_take_cancel(), Some(id));
        drop(stop);
        runner.finish(id, Ok(())).unwrap();
        assert!(runner.begin().is_ok());
    }

    #[test]
    fn explicit_lifecycle_stop_preserves_backend_error() {
        let state = Box::leak(Box::new(LifecycleState::<u32>::new()));
        let mut runner = state.claim().unwrap();
        let id = runner.begin().unwrap();
        let guard = runner.activate(id).unwrap();
        let mut stop = core::pin::pin!(guard.stop());
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(stop.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(runner.try_take_cancel(), Some(id));
        runner.finish(id, Err(0x1234)).unwrap();
        assert_eq!(
            stop.as_mut().poll(&mut context),
            Poll::Ready(Err(LifecycleStopError::Backend(0x1234)))
        );
    }

    #[test]
    fn lifecycle_generation_and_transitions_fail_closed() {
        let state = Box::leak(Box::new(LifecycleState::<u32>::new()));
        let mut runner = state.claim().unwrap();
        let first = runner.begin().unwrap();
        assert_eq!(runner.begin(), Err(LifecycleStateError::Busy));
        assert_eq!(runner.abort_start(first), Ok(()));
        let second = runner.begin().unwrap();
        assert_ne!(first, second);
        assert!(matches!(
            runner.activate(first),
            Err(LifecycleStateError::Stale)
        ));
        let guard = runner.activate(second).unwrap();
        assert_eq!(
            runner.finish(first, Ok(())),
            Err(LifecycleStateError::Stale)
        );
        drop(guard);
        assert_eq!(runner.try_take_cancel(), Some(second));
        runner.finish(second, Ok(())).unwrap();
    }
}
