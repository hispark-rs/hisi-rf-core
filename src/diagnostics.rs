//! Allocation-free, secret-free diagnostics for the public radio facade.

use core::fmt;

use crate::{BackendError, BackendErrorClass, Error};

/// Versioned machine-readable diagnostic schema.
pub const DIAGNOSTIC_SCHEMA: &str = "hisi-rf-error/v1";

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
    /// A bounded backend operation whose protocol-specific stage is unknown.
    Operation,
    /// A backend-specific stage that is not represented by the v1 schema.
    Backend,
}

impl DiagnosticStage {
    /// Stable machine-readable stage name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ControlPlane => "control_plane",
            Self::Connect => "connect",
            Self::Operation => "operation",
            Self::Backend => "backend",
        }
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
        write!(output, ",\"docs\":\"{}\"}}", self.docs_anchor())
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
            },
            Self::Backend(error) => error.diagnostic(),
            Self::Protocol => Diagnostic {
                code: DiagnosticCode::Protocol,
                stage: DiagnosticStage::ControlPlane,
                action: RecoveryAction::RecreateController,
                backend_code: None,
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
        let (code, stage, action) = match self.class {
            BackendErrorClass::Initialize => (
                DiagnosticCode::BackendInitialize,
                DiagnosticStage::Initialize,
                RecoveryAction::Reinitialize,
            ),
            BackendErrorClass::Busy => (
                DiagnosticCode::BackendBusy,
                DiagnosticStage::Operation,
                RecoveryAction::WaitAndRetry,
            ),
            BackendErrorClass::Timeout => (
                DiagnosticCode::BackendTimeout,
                DiagnosticStage::Operation,
                RecoveryAction::RetryOperation,
            ),
            BackendErrorClass::UnsupportedSecurity => (
                DiagnosticCode::UnsupportedSecurity,
                DiagnosticStage::Connect,
                RecoveryAction::SelectSupportedSecurity,
            ),
            BackendErrorClass::Connect => (
                DiagnosticCode::ConnectionFailed,
                DiagnosticStage::Connect,
                RecoveryAction::InspectNetworkAndRetry,
            ),
            BackendErrorClass::Other => (
                DiagnosticCode::BackendOther,
                DiagnosticStage::Backend,
                RecoveryAction::InspectBackendCode,
            ),
        };
        Diagnostic {
            code,
            stage,
            action,
            backend_code: Some(self.code),
        }
    }
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
        let error = BackendError {
            class: BackendErrorClass::Other,
            code: 0xdeaf_0042,
        };
        let diagnostic = error.diagnostic();

        assert_eq!(diagnostic.code(), DiagnosticCode::BackendOther);
        assert_eq!(diagnostic.stage(), DiagnosticStage::Backend);
        assert_eq!(diagnostic.action(), RecoveryAction::InspectBackendCode);
        assert_eq!(diagnostic.backend_code(), Some(0xdeaf_0042));
    }

    #[test]
    fn json_is_deterministic_and_contains_no_configuration_text() {
        let mut json = String::new();
        Error::Backend(BackendError {
            class: BackendErrorClass::Timeout,
            code: 7,
        })
        .diagnostic()
        .write_json(&mut json)
        .unwrap();

        assert_eq!(
            json,
            "{\"schema\":\"hisi-rf-error/v1\",\"code\":\"backend.timeout\",\"stage\":\"operation\",\"action\":\"retry_operation\",\"backend_code\":7,\"docs\":\"errors-backend-timeout\"}"
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
}
