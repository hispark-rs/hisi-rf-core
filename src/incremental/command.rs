use core::num::NonZeroU32;

use super::OperationId;

/// Non-zero sequence assigned by the async control plane.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CommandSequence(NonZeroU32);

impl CommandSequence {
    /// Validate a raw controller sequence.
    pub const fn try_from_raw(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Stable numeric representation used by the existing completion channel.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// One bounded command waiting to start.
#[derive(Debug)]
pub struct PendingCommand<C> {
    sequence: CommandSequence,
    command: C,
}

impl<C> PendingCommand<C> {
    /// Pair a validated sequence with an owned command.
    pub const fn new(sequence: CommandSequence, command: C) -> Self {
        Self { sequence, command }
    }

    /// Controller sequence associated with this command.
    pub const fn sequence(&self) -> CommandSequence {
        self.sequence
    }

    /// Recover the owned command after arbitration.
    pub fn into_inner(self) -> C {
        self.command
    }
}

/// Reason a bounded command arbitration transition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandArbiterError {
    /// One pending command already occupies the bounded queue.
    Busy,
    /// The transition does not match the current start/active state.
    InvalidTransition,
    /// The supplied operation identity belongs to an older generation.
    StaleOperation,
}

/// Failed submission that returns ownership of the unqueued command.
#[derive(Debug)]
pub struct SubmitError<C> {
    reason: CommandArbiterError,
    command: PendingCommand<C>,
}

impl<C> SubmitError<C> {
    /// Stable rejection reason.
    pub const fn reason(&self) -> CommandArbiterError {
        self.reason
    }

    /// Recover the command that did not enter the bounded queue.
    pub fn into_command(self) -> PendingCommand<C> {
        self.command
    }
}

/// Next control-plane action selected by [`CommandArbiter`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandArbiterAction {
    /// No active or pending command exists.
    Idle,
    /// Take and start the pending command.
    StartPending(CommandSequence),
    /// A start call is in progress and must resolve before another action.
    Starting(CommandSequence),
    /// Ask the backend to cancel the active operation once.
    CancelActive(OperationId),
    /// Wait for the active operation to become terminal.
    WaitActive(OperationId),
}

#[derive(Clone, Copy, Debug)]
struct ActiveCommand {
    sequence: CommandSequence,
    operation: OperationId,
    cancel_requested: bool,
}

/// Bounded active/pending command ownership for the incremental runner.
///
/// The controller is serialized, but dropping one control future permits a new
/// command to enter while the old backend operation is still active. This
/// arbiter retains at most one replacement command, requests cancellation only
/// once, and never overwrites a command or operation identity.
#[derive(Debug)]
pub struct CommandArbiter<C> {
    active: Option<ActiveCommand>,
    starting: Option<CommandSequence>,
    pending: Option<PendingCommand<C>>,
}

impl<C> CommandArbiter<C> {
    /// Construct an empty arbiter.
    pub const fn new() -> Self {
        Self {
            active: None,
            starting: None,
            pending: None,
        }
    }

    /// Whether the one-entry pending command slot is available.
    ///
    /// Facade adapters use this as backpressure before removing another
    /// command from their own bounded channel.
    pub const fn can_submit(&self) -> bool {
        self.pending.is_none()
    }

    /// Queue one command without replacing an existing pending command.
    pub fn submit(&mut self, command: PendingCommand<C>) -> Result<(), SubmitError<C>> {
        if self.pending.is_some() {
            return Err(SubmitError {
                reason: CommandArbiterError::Busy,
                command,
            });
        }
        self.pending = Some(command);
        Ok(())
    }

    /// Select the next control-plane action.
    pub fn action(&self) -> CommandArbiterAction {
        if let Some(active) = self.active {
            if self.pending.is_some() && !active.cancel_requested {
                CommandArbiterAction::CancelActive(active.operation)
            } else {
                CommandArbiterAction::WaitActive(active.operation)
            }
        } else if let Some(sequence) = self.starting {
            CommandArbiterAction::Starting(sequence)
        } else if let Some(pending) = self.pending.as_ref() {
            CommandArbiterAction::StartPending(pending.sequence())
        } else {
            CommandArbiterAction::Idle
        }
    }

    /// Take the command selected by [`CommandArbiterAction::StartPending`].
    pub fn take_startable(&mut self) -> Result<PendingCommand<C>, CommandArbiterError> {
        if self.active.is_some() || self.starting.is_some() {
            return Err(CommandArbiterError::InvalidTransition);
        }
        let pending = self
            .pending
            .take()
            .ok_or(CommandArbiterError::InvalidTransition)?;
        self.starting = Some(pending.sequence());
        Ok(pending)
    }

    /// Bind a successful backend start to its operation identity.
    pub fn mark_started(
        &mut self,
        sequence: CommandSequence,
        operation: OperationId,
    ) -> Result<(), CommandArbiterError> {
        if self.starting != Some(sequence) || self.active.is_some() {
            return Err(CommandArbiterError::InvalidTransition);
        }
        self.starting = None;
        self.active = Some(ActiveCommand {
            sequence,
            operation,
            cancel_requested: false,
        });
        Ok(())
    }

    /// Clear a failed backend start so another pending command may proceed.
    pub fn reject_start(&mut self, sequence: CommandSequence) -> Result<(), CommandArbiterError> {
        if self.starting != Some(sequence) {
            return Err(CommandArbiterError::InvalidTransition);
        }
        self.starting = None;
        Ok(())
    }

    /// Record that the one cancellation request was sent to the backend.
    pub fn mark_cancel_requested(
        &mut self,
        operation: OperationId,
    ) -> Result<(), CommandArbiterError> {
        let active = self
            .active
            .as_mut()
            .ok_or(CommandArbiterError::StaleOperation)?;
        if active.operation != operation {
            return Err(CommandArbiterError::StaleOperation);
        }
        if active.cancel_requested {
            return Err(CommandArbiterError::InvalidTransition);
        }
        active.cancel_requested = true;
        Ok(())
    }

    /// Finish exactly the current operation and free the active slot.
    pub fn finish_active(
        &mut self,
        operation: OperationId,
    ) -> Result<CommandSequence, CommandArbiterError> {
        let active = self.active.ok_or(CommandArbiterError::StaleOperation)?;
        if active.operation != operation {
            return Err(CommandArbiterError::StaleOperation);
        }
        self.active = None;
        Ok(active.sequence)
    }

    /// Current active command and backend operation identity.
    pub const fn active(&self) -> Option<(CommandSequence, OperationId)> {
        match self.active {
            Some(active) => Some((active.sequence, active.operation)),
            None => None,
        }
    }

    /// Sequence retained in the one-entry pending queue.
    pub fn pending_sequence(&self) -> Option<CommandSequence> {
        self.pending.as_ref().map(PendingCommand::sequence)
    }
}

impl<C> Default for CommandArbiter<C> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IncrementalRunnerState, OperationStateError, RunnerStateError};

    fn sequence(value: u32) -> CommandSequence {
        CommandSequence::try_from_raw(value).unwrap()
    }

    fn operation(runner: &mut IncrementalRunnerState) -> OperationId {
        let operation = runner.queue(0).unwrap();
        runner.mark_started(operation).unwrap();
        operation
    }

    #[test]
    fn replacement_command_cancels_active_once_then_waits() {
        let mut runner = IncrementalRunnerState::new();
        let first_operation = operation(&mut runner);
        let mut arbiter = CommandArbiter::new();

        arbiter
            .submit(PendingCommand::new(sequence(1), "scan"))
            .unwrap();
        let first = arbiter.take_startable().unwrap();
        arbiter
            .mark_started(first.sequence(), first_operation)
            .unwrap();
        assert_eq!(first.into_inner(), "scan");

        arbiter
            .submit(PendingCommand::new(sequence(2), "disconnect"))
            .unwrap();
        assert_eq!(
            arbiter.action(),
            CommandArbiterAction::CancelActive(first_operation)
        );
        arbiter.mark_cancel_requested(first_operation).unwrap();
        assert_eq!(
            arbiter.action(),
            CommandArbiterAction::WaitActive(first_operation)
        );
        assert_eq!(arbiter.finish_active(first_operation), Ok(sequence(1)));
        assert_eq!(
            arbiter.action(),
            CommandArbiterAction::StartPending(sequence(2))
        );
    }

    #[test]
    fn bounded_pending_queue_returns_the_rejected_command() {
        let mut arbiter = CommandArbiter::new();
        arbiter
            .submit(PendingCommand::new(sequence(1), 10))
            .unwrap();
        let error = arbiter
            .submit(PendingCommand::new(sequence(2), 20))
            .unwrap_err();
        assert_eq!(error.reason(), CommandArbiterError::Busy);
        assert_eq!(error.into_command().into_inner(), 20);
        assert_eq!(arbiter.pending_sequence(), Some(sequence(1)));
    }

    #[test]
    fn stale_terminal_cannot_finish_a_reused_operation() {
        let mut runner = IncrementalRunnerState::new();
        let first_operation = operation(&mut runner);
        runner
            .apply_error(
                first_operation,
                crate::BackendError::new(crate::BackendErrorClass::Other, 1),
            )
            .unwrap();
        runner.reap(first_operation).unwrap();
        let second_operation = operation(&mut runner);

        let mut arbiter = CommandArbiter::new();
        arbiter
            .submit(PendingCommand::new(sequence(2), ()))
            .unwrap();
        let second = arbiter.take_startable().unwrap();
        arbiter
            .mark_started(second.sequence(), second_operation)
            .unwrap();
        assert_eq!(
            arbiter.finish_active(first_operation),
            Err(CommandArbiterError::StaleOperation)
        );
        assert_eq!(arbiter.active(), Some((sequence(2), second_operation)));
    }

    #[test]
    fn rejected_start_does_not_poison_the_next_command() {
        let mut arbiter = CommandArbiter::new();
        arbiter
            .submit(PendingCommand::new(sequence(1), "first"))
            .unwrap();
        let first = arbiter.take_startable().unwrap();
        assert_eq!(
            arbiter.action(),
            CommandArbiterAction::Starting(sequence(1))
        );
        arbiter.reject_start(first.sequence()).unwrap();
        arbiter
            .submit(PendingCommand::new(sequence(2), "second"))
            .unwrap();
        assert_eq!(
            arbiter.action(),
            CommandArbiterAction::StartPending(sequence(2))
        );
    }

    #[test]
    fn zero_sequence_and_stale_runner_identity_fail_closed() {
        assert_eq!(CommandSequence::try_from_raw(0), None);
        let mut runner = IncrementalRunnerState::new();
        let stale = runner.queue(0).unwrap();
        runner.mark_started(stale).unwrap();
        runner
            .apply_error(
                stale,
                crate::BackendError::new(crate::BackendErrorClass::Other, 1),
            )
            .unwrap();
        runner.reap(stale).unwrap();
        assert_eq!(
            runner.mark_started(stale),
            Err(RunnerStateError::Operation(OperationStateError::Stale))
        );
    }
}
