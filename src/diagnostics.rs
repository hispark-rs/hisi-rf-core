//! Allocation-free, secret-free diagnostics for the public radio facade.

use core::fmt;

use crate::{BackendError, BackendErrorClass, Error};

/// Versioned machine-readable diagnostic schema.
pub const DIAGNOSTIC_SCHEMA: &str = "hisi-rf-error/v2";

/// Maximum number of backend trace entries retained by one public error.
pub const DIAGNOSTIC_TRACE_CAPACITY: usize = 4;

/// Stable identity for a public RF failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// The caller tried to claim an already-owned radio state.
    AlreadyInitialized,
    /// A backend initialization step failed.
    BackendInitialize,
    /// Another operation currently owns the backend.
    BackendBusy,
    /// A bounded backend operation timed out.
    BackendTimeout,
    /// The selected security mode is unsupported by this build or target.
    UnsupportedSecurity,
    /// Association or authorization failed.
    ConnectionFailed,
    /// A backend-specific failure has no more specific stable classification.
    BackendOther,
    /// The runner observed an invalid command/completion sequence.
    Protocol,
}

impl DiagnosticCode {
    /// Stable identifier used by JSON output, logs, and support tooling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "radio.already_initialized",
            Self::BackendInitialize => "backend.initialize",
            Self::BackendBusy => "backend.busy",
            Self::BackendTimeout => "backend.timeout",
            Self::UnsupportedSecurity => "wifi.unsupported_security",
            Self::ConnectionFailed => "wifi.connection_failed",
            Self::BackendOther => "backend.other",
            Self::Protocol => "radio.protocol",
        }
    }
}

/// Stable stage at which a public RF failure was reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticStage {
    /// Radio/backend construction and initialization.
    Initialize,
    /// Radio ownership or runner command/completion handling.
    ControlPlane,
    /// Wi-Fi association or authorization.
    Connect,
    /// Wi-Fi scan and BSS discovery.
    Scan,
    /// 802.11 authentication management exchange.
    Authenticate,
    /// 802.11 association exchange.
    Associate,
    /// WPA3 SAE external-auth exchange.
    Sae,
    /// WPA EAPOL key exchange.
    Eapol,
    /// Protected-management-frame negotiation or recovery.
    Pmf,
    /// Explicit disconnect/deauthentication handling.
    Disconnect,
    /// Runtime scheduling, wait, or IPC service.
    Runtime,
    /// A bounded backend operation whose protocol-specific stage is unknown.
    Operation,
    /// A backend-specific stage not represented by the current schema.
    Backend,
}

impl DiagnosticStage {
    /// Stable machine-readable stage name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ControlPlane => "control_plane",
            Self::Connect => "connect",
            Self::Scan => "scan",
            Self::Authenticate => "authenticate",
            Self::Associate => "associate",
            Self::Sae => "sae",
            Self::Eapol => "eapol",
            Self::Pmf => "pmf",
            Self::Disconnect => "disconnect",
            Self::Runtime => "runtime",
            Self::Operation => "operation",
            Self::Backend => "backend",
        }
    }
}

/// Stable identity for one bounded backend trace value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTraceKind {
    /// Raw status returned by the backend or protocol engine.
    BackendStatus,
    /// IEEE 802.11 status code from authentication/association.
    IeeeStatus,
    /// Wi-Fi disconnect reason.
    DisconnectReason,
    /// Upstream hostap context state snapshot.
    SupplicantContext,
    /// WS63 driver/port state snapshot.
    DriverContext,
    /// Runtime service error code.
    RuntimeCode,
}

impl DiagnosticTraceKind {
    /// Stable machine-readable trace field name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BackendStatus => "backend_status",
            Self::IeeeStatus => "ieee_status",
            Self::DisconnectReason => "disconnect_reason",
            Self::SupplicantContext => "supplicant_context",
            Self::DriverContext => "driver_context",
            Self::RuntimeCode => "runtime_code",
        }
    }
}

/// One numeric, secret-free backend trace entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticTraceEntry {
    kind: DiagnosticTraceKind,
    value: u32,
}

impl DiagnosticTraceEntry {
    /// Stable identity of this value.
    pub const fn kind(self) -> DiagnosticTraceKind {
        self.kind
    }

    /// Lossless numeric value.
    pub const fn value(self) -> u32 {
        self.value
    }
}

/// Fixed-capacity trace attached to one backend error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticTrace {
    entries: [Option<DiagnosticTraceEntry>; DIAGNOSTIC_TRACE_CAPACITY],
    len: u8,
    truncated: bool,
}

impl DiagnosticTrace {
    /// Create an empty trace.
    pub const fn new() -> Self {
        Self {
            entries: [None; DIAGNOSTIC_TRACE_CAPACITY],
            len: 0,
            truncated: false,
        }
    }

    /// Number of retained entries.
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Whether no trace entry is present.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Whether additional backend entries were dropped at the fixed limit.
    pub const fn is_truncated(self) -> bool {
        self.truncated
    }

    /// Return an entry by index.
    pub const fn get(self, index: usize) -> Option<DiagnosticTraceEntry> {
        if index < self.len as usize {
            self.entries[index]
        } else {
            None
        }
    }

    pub(crate) fn push(&mut self, kind: DiagnosticTraceKind, value: u32) {
        let index = self.len as usize;
        if index < DIAGNOSTIC_TRACE_CAPACITY {
            self.entries[index] = Some(DiagnosticTraceEntry { kind, value });
            self.len += 1;
        } else {
            self.truncated = true;
        }
    }
}

impl Default for DiagnosticTrace {
    fn default() -> Self {
        Self::new()
    }
}

/// Action a caller or operator can take after a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Keep using the controller that already owns the supplied state.
    UseExistingController,
    /// Recreate the radio controller and reinitialize the backend.
    Reinitialize,
    /// Wait for the current operation to finish before retrying.
    WaitAndRetry,
    /// Retry the bounded operation; repeated failures require deeper inspection.
    RetryOperation,
    /// Select a security profile supported by the target and build.
    SelectSupportedSecurity,
    /// Inspect network status and the lossless backend code before retrying.
    InspectNetworkAndRetry,
    /// Preserve and report the backend code with target/profile information.
    InspectBackendCode,
    /// Recreate the controller and report a repeated protocol failure.
    RecreateController,
}

impl RecoveryAction {
    /// Stable machine-readable action name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UseExistingController => "use_existing_controller",
            Self::Reinitialize => "reinitialize",
            Self::WaitAndRetry => "wait_and_retry",
            Self::RetryOperation => "retry_operation",
            Self::SelectSupportedSecurity => "select_supported_security",
            Self::InspectNetworkAndRetry => "inspect_network_and_retry",
            Self::InspectBackendCode => "inspect_backend_code",
            Self::RecreateController => "recreate_controller",
        }
    }
}

/// Allocation-free diagnostic view of a public RF [`Error`].
///
/// The view intentionally contains no SSID, passphrase, key material, or
/// arbitrary backend text. Unknown chip failures retain their numeric code so
/// agent and support tooling can identify them without losing information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    stage: DiagnosticStage,
    action: RecoveryAction,
    backend_code: Option<u32>,
    profile_revision: Option<&'static str>,
    trace: DiagnosticTrace,
}

impl Diagnostic {
    /// Schema used by [`Self::write_json`].
    pub const fn schema(self) -> &'static str {
        DIAGNOSTIC_SCHEMA
    }

    /// Stable error identity.
    pub const fn code(self) -> DiagnosticCode {
        self.code
    }

    /// Stable operation stage.
    pub const fn stage(self) -> DiagnosticStage {
        self.stage
    }

    /// Recommended next action.
    pub const fn action(self) -> RecoveryAction {
        self.action
    }

    /// Lossless chip/backend-specific code, when the failure came from a backend.
    pub const fn backend_code(self) -> Option<u32> {
        self.backend_code
    }

    /// Backend/profile revision that produced this failure.
    pub const fn profile_revision(self) -> Option<&'static str> {
        self.profile_revision
    }

    /// Bounded, numeric backend trace.
    pub const fn trace(self) -> DiagnosticTrace {
        self.trace
    }

    /// Stable documentation fragment for this diagnostic.
    pub const fn docs_anchor(self) -> &'static str {
        match self.code {
            DiagnosticCode::AlreadyInitialized => "errors-radio-already-initialized",
            DiagnosticCode::BackendInitialize => "errors-backend-initialize",
            DiagnosticCode::BackendBusy => "errors-backend-busy",
            DiagnosticCode::BackendTimeout => "errors-backend-timeout",
            DiagnosticCode::UnsupportedSecurity => "errors-wifi-unsupported-security",
            DiagnosticCode::ConnectionFailed => "errors-wifi-connection-failed",
            DiagnosticCode::BackendOther => "errors-backend-other",
            DiagnosticCode::Protocol => "errors-radio-protocol",
        }
    }

    /// Write one deterministic JSON object without allocation.
    pub fn write_json(self, output: &mut impl fmt::Write) -> fmt::Result {
        write!(
            output,
            "{{\"schema\":\"{}\",\"code\":\"{}\",\"stage\":\"{}\",\"action\":\"{}\",\"backend_code\":",
            self.schema(),
            self.code.as_str(),
            self.stage.as_str(),
            self.action.as_str(),
        )?;
        match self.backend_code {
            Some(code) => write!(output, "{code}"),
            None => output.write_str("null"),
        }?;
        output.write_str(",\"profile_revision\":")?;
        match self.profile_revision {
            Some(revision) => write_json_string(output, revision)?,
            None => output.write_str("null")?,
        }
        output.write_str(",\"trace\":[")?;
        for index in 0..self.trace.len() {
            if index != 0 {
                output.write_str(",")?;
            }
            let entry = self.trace.get(index).expect("trace length is bounded");
            write!(
                output,
                "{{\"kind\":\"{}\",\"value\":{}}}",
                entry.kind().as_str(),
                entry.value()
            )?;
        }
        write!(
            output,
            "],\"trace_truncated\":{},\"docs\":\"{}\"}}",
            self.trace.is_truncated(),
            self.docs_anchor()
        )
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}; next action: {}",
            self.code.as_str(),
            self.stage.as_str(),
            self.action.as_str(),
        )?;
        if let Some(code) = self.backend_code {
            write!(formatter, "; backend code: 0x{code:08x}")?;
        }
        Ok(())
    }
}

impl Error {
    /// Convert this error into the stable diagnostic schema.
    pub const fn diagnostic(self) -> Diagnostic {
        match self {
            Self::AlreadyInitialized => Diagnostic {
                code: DiagnosticCode::AlreadyInitialized,
                stage: DiagnosticStage::ControlPlane,
                action: RecoveryAction::UseExistingController,
                backend_code: None,
                profile_revision: None,
                trace: DiagnosticTrace::new(),
            },
            Self::Backend(error) => error.diagnostic(),
            Self::Protocol => Diagnostic {
                code: DiagnosticCode::Protocol,
                stage: DiagnosticStage::ControlPlane,
                action: RecoveryAction::RecreateController,
                backend_code: None,
                profile_revision: None,
                trace: DiagnosticTrace::new(),
            },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic().fmt(formatter)
    }
}

impl BackendError {
    /// Convert this backend error into the stable diagnostic schema.
    pub const fn diagnostic(self) -> Diagnostic {
        let (code, action) = match self.class() {
            BackendErrorClass::Initialize => (
                DiagnosticCode::BackendInitialize,
                RecoveryAction::Reinitialize,
            ),
            BackendErrorClass::Busy => (DiagnosticCode::BackendBusy, RecoveryAction::WaitAndRetry),
            BackendErrorClass::Timeout => (
                DiagnosticCode::BackendTimeout,
                RecoveryAction::RetryOperation,
            ),
            BackendErrorClass::UnsupportedSecurity => (
                DiagnosticCode::UnsupportedSecurity,
                RecoveryAction::SelectSupportedSecurity,
            ),
            BackendErrorClass::Connect => (
                DiagnosticCode::ConnectionFailed,
                RecoveryAction::InspectNetworkAndRetry,
            ),
            BackendErrorClass::Other => (
                DiagnosticCode::BackendOther,
                RecoveryAction::InspectBackendCode,
            ),
        };
        Diagnostic {
            code,
            stage: self.stage(),
            action,
            backend_code: Some(self.code()),
            profile_revision: self.profile_revision(),
            trace: self.trace(),
        }
    }
}

fn write_json_string(output: &mut impl fmt::Write, value: &str) -> fmt::Result {
    output.write_str("\"")?;
    for character in value.chars() {
        match character {
            '\"' => output.write_str("\\\"")?,
            '\\' => output.write_str("\\\\")?,
            '\n' => output.write_str("\\n")?,
            '\r' => output.write_str("\\r")?,
            '\t' => output.write_str("\\t")?,
            control if control.is_control() => write!(output, "\\u{:04x}", control as u32)?,
            character => output.write_char(character)?,
        }
    }
    output.write_str("\"")
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic().fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::String;

    use super::*;

    #[test]
    fn unknown_backend_codes_remain_lossless_and_actionable() {
        let error = BackendError::new(BackendErrorClass::Other, 0xdeaf_0042);
        let diagnostic = error.diagnostic();

        assert_eq!(diagnostic.code(), DiagnosticCode::BackendOther);
        assert_eq!(diagnostic.stage(), DiagnosticStage::Backend);
        assert_eq!(diagnostic.action(), RecoveryAction::InspectBackendCode);
        assert_eq!(diagnostic.backend_code(), Some(0xdeaf_0042));
    }

    #[test]
    fn json_is_deterministic_and_contains_no_configuration_text() {
        let mut json = String::new();
        Error::Backend(BackendError::new(BackendErrorClass::Timeout, 7))
            .diagnostic()
            .write_json(&mut json)
            .unwrap();

        assert_eq!(
            json,
            "{\"schema\":\"hisi-rf-error/v2\",\"code\":\"backend.timeout\",\"stage\":\"operation\",\"action\":\"retry_operation\",\"backend_code\":7,\"profile_revision\":null,\"trace\":[],\"trace_truncated\":false,\"docs\":\"errors-backend-timeout\"}"
        );
        assert!(!json.contains("ssid"));
        assert!(!json.contains("passphrase"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn local_errors_do_not_invent_backend_codes() {
        let diagnostic = Error::AlreadyInitialized.diagnostic();

        assert_eq!(diagnostic.backend_code(), None);
        assert_eq!(diagnostic.action(), RecoveryAction::UseExistingController);
        assert_eq!(diagnostic.docs_anchor(), "errors-radio-already-initialized");
    }

    #[test]
    fn backend_context_is_bounded_escaped_and_secret_free() {
        let diagnostic = BackendError::new(BackendErrorClass::Connect, 30)
            .with_stage(DiagnosticStage::Pmf)
            .with_profile_revision("ws63-\"profile")
            .with_trace(DiagnosticTraceKind::IeeeStatus, 30)
            .with_trace(DiagnosticTraceKind::SupplicantContext, 0x445)
            .diagnostic();
        let mut json = String::new();
        diagnostic.write_json(&mut json).unwrap();

        assert_eq!(diagnostic.stage(), DiagnosticStage::Pmf);
        assert_eq!(diagnostic.profile_revision(), Some("ws63-\"profile"));
        assert_eq!(diagnostic.trace().len(), 2);
        assert!(json.contains("ws63-\\\"profile"));
        assert!(json.contains("\"kind\":\"ieee_status\",\"value\":30"));
        assert!(!json.contains("ssid"));
        assert!(!json.contains("passphrase"));
    }

    #[test]
    fn trace_reports_capacity_truncation() {
        let diagnostic = BackendError::new(BackendErrorClass::Other, 9)
            .with_trace(DiagnosticTraceKind::BackendStatus, 1)
            .with_trace(DiagnosticTraceKind::BackendStatus, 2)
            .with_trace(DiagnosticTraceKind::BackendStatus, 3)
            .with_trace(DiagnosticTraceKind::BackendStatus, 4)
            .with_trace(DiagnosticTraceKind::BackendStatus, 5)
            .diagnostic();
        assert_eq!(diagnostic.trace().len(), DIAGNOSTIC_TRACE_CAPACITY);
        assert!(diagnostic.trace().is_truncated());
    }
}
