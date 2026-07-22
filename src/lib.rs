//! Chip-neutral radio control and L2 data-plane contracts.
//!
//! This crate owns the portable controller, runner, configuration, event, and
//! L2-device contracts. Applications normally depend on the chip-selecting
//! `hisi-rf` facade instead of naming this implementation crate directly.
//!
//! [`init`] claims caller-provided static state and returns an exclusive
//! [`RadioController`]. Splitting it yields a [`WifiController`], a
//! [`WifiDevice`], and the mandatory [`RadioRunner`]. Only the runner calls the
//! chip backend; control methods merely enqueue commands and await completion.

#![no_std]

mod diagnostics;
#[cfg(feature = "incremental-backend-experiment")]
mod incremental;
mod state;
mod wifi;

pub use diagnostics::{
    DIAGNOSTIC_SCHEMA, DIAGNOSTIC_TRACE_CAPACITY, Diagnostic, DiagnosticCode, DiagnosticStage,
    DiagnosticTrace, DiagnosticTraceEntry, DiagnosticTraceKind, RecoveryAction,
};
#[cfg(feature = "incremental-backend-experiment")]
pub use incremental::{
    CancelDirective, CancelOutcome, CommandArbiter, CommandArbiterAction, CommandArbiterError,
    CommandSequence, FairWakeSelector, IncrementalBackendDriver, IncrementalCompletion,
    IncrementalDriverError, IncrementalDriverEvent, IncrementalRadioParts, IncrementalRadioRunner,
    IncrementalRadioRunnerError, IncrementalRequest, IncrementalRunnerState,
    IncrementalWifiBackend, OperationId, OperationLifecycle, OperationStateError, OperationTracker,
    PendingCommand, PollDisposition, RunnerStateError, RunnerStep, RunnerTransition, SubmitError,
    WaitSet, WakeReason, WorkBudget, WorkReport,
};
pub use wifi::{
    BackendError, BackendErrorClass, ConnectionInfo, EventDiagnostics, ManagementFrameProtection,
    Passphrase, PersonalSecurity, RadioConfig, RadioController, RadioParts, RadioResources,
    RadioRunner, RadioState, SaePwe, ScanConfig, ScanOutcome, ScanResult, Security, Ssid,
    StationConfig, WifiBackend, WifiConfig, WifiController, WifiDevice, WifiEvent, WifiParts, init,
};

/// Failure to establish or use the radio control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The supplied [`RadioState`] has already been claimed.
    AlreadyInitialized,
    /// The chip backend rejected an operation.
    Backend(BackendError),
    /// A backend completion did not match the outstanding command.
    Protocol,
}
