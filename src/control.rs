//! Internal bounded controller-to-runner command transport.
//!
//! This module is public only so independently released facade and chip crates
//! can share one implementation. Applications should use `hisi-rf` protocol
//! handles instead of naming these transport types.

use core::num::NonZeroU32;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, Ordering};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;

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
}
