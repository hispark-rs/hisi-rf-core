use portable_atomic::Ordering;

use crate::Error;
use crate::state::SharedState;

pub mod security;
pub use security::{ManagementFrameProtection, PersonalSecurity, SaePwe};

pub(crate) const MAX_SCAN_RESULTS: usize = 32;
const SSID_CAPACITY: usize = 32;
const PASSPHRASE_CAPACITY: usize = 63;

/// Radio-wide configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct RadioConfig {
    /// Wi-Fi control-plane defaults.
    pub wifi: WifiConfig,
}

/// Wi-Fi control-plane defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiConfig {
    /// Maximum time a backend may spend initializing.
    pub initialize_timeout_ms: u32,
    /// Maximum time to wait for a disconnect event.
    pub disconnect_timeout_ms: u32,
}

impl Default for WifiConfig {
    fn default() -> Self {
        Self {
            initialize_timeout_ms: 30_000,
            disconnect_timeout_ms: 10_000,
        }
    }
}

/// Validated IEEE 802.11 SSID bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ssid {
    bytes: [u8; SSID_CAPACITY],
    len: u8,
}

impl Ssid {
    /// Validate and copy a non-empty SSID of at most 32 bytes.
    pub fn try_from_bytes(value: &[u8]) -> Option<Self> {
        if value.is_empty() || value.len() > SSID_CAPACITY {
            return None;
        }
        let mut bytes = [0; SSID_CAPACITY];
        bytes[..value.len()].copy_from_slice(value);
        Some(Self {
            bytes,
            len: value.len() as u8,
        })
    }

    /// Return the exact SSID bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// Owned WPA2/WPA3-Personal passphrase that is erased on drop.
#[derive(Debug, Eq, PartialEq)]
pub struct Passphrase {
    bytes: [u8; PASSPHRASE_CAPACITY],
    len: u8,
}

impl Passphrase {
    /// Validate and copy an 8-63 byte printable ASCII passphrase.
    pub fn try_from_ascii(value: &[u8]) -> Option<Self> {
        if !(8..=PASSPHRASE_CAPACITY).contains(&value.len())
            || value.iter().any(|byte| *byte < 32 || *byte == 127)
        {
            return None;
        }
        let mut bytes = [0; PASSPHRASE_CAPACITY];
        bytes[..value.len()].copy_from_slice(value);
        Some(Self {
            bytes,
            len: value.len() as u8,
        })
    }

    /// Borrow the passphrase bytes for a backend call.
    pub fn expose_secret(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl Drop for Passphrase {
    fn drop(&mut self) {
        for byte in &mut self.bytes {
            // Volatile stores keep secret erasure observable to the compiler.
            // SAFETY: `byte` uniquely borrows one live element of this owned
            // array and is valid for a one-byte volatile write.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        self.len = 0;
    }
}

/// Link-layer security discovered during scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    /// Open network.
    Open,
    /// WPA2-Personal with CCMP.
    Wpa2Personal,
    /// WPA3-Personal with SAE and mandatory PMF.
    Wpa3Personal,
    /// A protected mode not yet represented by this public API.
    OtherProtected,
}

/// One chip-neutral bounded scan result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanResult {
    /// Advertised SSID.
    pub ssid: Ssid,
    /// Basic service set identifier.
    pub bssid: [u8; 6],
    /// Center frequency in MHz.
    pub frequency_mhz: u16,
    /// Signal strength in dBm.
    pub rssi_dbm: i16,
    /// Link-layer security class.
    pub security: Security,
    /// Primary channel when known, otherwise zero.
    pub channel: u8,
}

impl ScanResult {
    pub(crate) const EMPTY: Self = Self {
        ssid: Ssid {
            bytes: [0; SSID_CAPACITY],
            len: 0,
        },
        bssid: [0; 6],
        frequency_mhz: 0,
        rssi_dbm: 0,
        security: Security::Open,
        channel: 0,
    };

    /// Empty value for caller-provided fixed scan buffers.
    pub const fn empty() -> Self {
        Self::EMPTY
    }
}

/// Bounded station scan request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanConfig {
    timeout_ms: u32,
}

impl ScanConfig {
    /// Construct a scan request with a non-zero timeout.
    pub const fn try_from_timeout_ms(timeout_ms: u32) -> Option<Self> {
        if timeout_ms == 0 {
            None
        } else {
            Some(Self { timeout_ms })
        }
    }

    /// Timeout passed to the chip backend.
    pub const fn timeout_ms(self) -> u32 {
        self.timeout_ms
    }
}

/// Result count for a caller-provided scan buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanOutcome {
    /// Entries copied into the caller's buffer.
    pub count: usize,
    /// At least one result did not fit in the backend or caller buffer.
    pub truncated: bool,
}

/// Validated station connection request.
#[derive(Debug, Eq, PartialEq)]
pub struct StationConfig {
    /// SSID selected by the application.
    pub ssid: Ssid,
    /// BSSID selected by the immediately preceding scan.
    pub bssid: [u8; 6],
    /// Primary channel selected by the immediately preceding scan.
    pub channel: u8,
    /// WPA2/WPA3-Personal passphrase.
    pub passphrase: Passphrase,
    security: PersonalSecurity,
    timeout_ms: u32,
}

impl StationConfig {
    /// Select a WPA2-Personal scan result and take ownership of its passphrase.
    pub fn wpa2_personal(
        result: &ScanResult,
        passphrase: Passphrase,
        timeout_ms: u32,
    ) -> Option<Self> {
        if result.security != Security::Wpa2Personal || timeout_ms == 0 {
            return None;
        }
        Some(Self {
            ssid: result.ssid,
            bssid: result.bssid,
            channel: result.channel,
            passphrase,
            security: PersonalSecurity::Wpa2,
            timeout_ms,
        })
    }

    /// Select a WPA3-Personal scan result and take ownership of its passphrase.
    ///
    /// PMF is mandatory by construction; callers only choose the SAE
    /// password-element policy supported by their controlled deployment.
    pub fn wpa3_personal(
        result: &ScanResult,
        passphrase: Passphrase,
        sae_pwe: SaePwe,
        timeout_ms: u32,
    ) -> Option<Self> {
        if result.security != Security::Wpa3Personal || timeout_ms == 0 {
            return None;
        }
        Some(Self {
            ssid: result.ssid,
            bssid: result.bssid,
            channel: result.channel,
            passphrase,
            security: PersonalSecurity::Wpa3 { sae_pwe },
            timeout_ms,
        })
    }

    /// Typed Personal-mode security consumed by the chip backend.
    pub const fn security(&self) -> PersonalSecurity {
        self.security
    }

    /// Maximum time to wait for association and authorization.
    pub const fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }
}

/// Successful station association.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    /// Associated BSSID.
    pub bssid: [u8; 6],
    /// Associated center frequency in MHz.
    pub frequency_mhz: u16,
}

/// Stable class for backend-specific failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendErrorClass {
    /// Radio initialization failed.
    Initialize,
    /// The requested operation is already active.
    Busy,
    /// A bounded operation timed out.
    Timeout,
    /// The requested security mode is unsupported.
    UnsupportedSecurity,
    /// Association or authorization failed.
    Connect,
    /// A chip-specific failure outside the stable classes.
    Other,
}

/// Backend error with a stable class and lossless chip-specific code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendError {
    /// Stable failure class.
    pub class: BackendErrorClass,
    /// Chip/backend-specific diagnostic code.
    pub code: u32,
}

/// Successful and failed state transitions emitted by the runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiEvent {
    /// Backend initialization completed.
    Initialized,
    /// Scan completed and retained this many results.
    ScanCompleted { count: usize, truncated: bool },
    /// Station association and authorization completed.
    Connected(ConnectionInfo),
    /// The station disconnected.
    Disconnected { reason: u16 },
    /// An operation failed in the backend.
    Failed(BackendError),
}

/// Event queue overflow diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventDiagnostics {
    /// Compile-time queue depth.
    pub capacity: usize,
    /// Events currently waiting for the controller.
    pub pending: usize,
    /// Oldest events discarded because the queue was full.
    pub dropped: u32,
}

/// Chip backend driven exclusively by [`RadioRunner`].
pub trait WifiBackend {
    /// Initialize the vendor/ROM radio runtime.
    fn initialize(&mut self, config: &WifiConfig) -> Result<(), BackendError>;

    /// Scan into the fixed runner-owned buffer.
    fn scan(
        &mut self,
        config: ScanConfig,
        output: &mut [ScanResult],
    ) -> Result<ScanOutcome, BackendError>;

    /// Associate and authorize one station connection.
    fn connect(&mut self, config: &StationConfig) -> Result<ConnectionInfo, BackendError>;

    /// Disconnect the station interface.
    fn disconnect(&mut self, config: &WifiConfig) -> Result<(), BackendError>;

    /// Advance bounded background work owned by the radio runner.
    ///
    /// Push-only backends may keep the default implementation. Host-side
    /// protocol engines use this seam for event-loop deadlines and queued RX;
    /// it must not invoke application callbacks.
    fn poll(&mut self) -> Result<bool, BackendError> {
        Ok(false)
    }
}

/// Caller-provided chip resources.
pub struct RadioResources<B, D> {
    /// Control-plane backend moved into the runner.
    pub backend: B,
    /// L2 device moved into the Wi-Fi data plane.
    pub device: D,
}

/// Static storage for one radio controller and its bounded event queue.
pub struct RadioState<const EVENTS: usize> {
    shared: SharedState<EVENTS>,
}

impl<const EVENTS: usize> RadioState<EVENTS> {
    /// Construct unclaimed radio state suitable for static allocation.
    pub const fn new() -> Self {
        Self {
            shared: SharedState::new(),
        }
    }
}

impl<const EVENTS: usize> Default for RadioState<EVENTS> {
    fn default() -> Self {
        Self::new()
    }
}

/// Exclusive unsplit radio ownership.
pub struct RadioController<B, D, const EVENTS: usize> {
    config: RadioConfig,
    resources: RadioResources<B, D>,
    state: &'static RadioState<EVENTS>,
}

/// Claim one radio instance without invoking its backend.
pub fn init<B, D, const EVENTS: usize>(
    config: RadioConfig,
    resources: RadioResources<B, D>,
    state: &'static RadioState<EVENTS>,
) -> Result<RadioController<B, D, EVENTS>, Error> {
    if !state.shared.claim() {
        return Err(Error::AlreadyInitialized);
    }
    Ok(RadioController {
        config,
        resources,
        state,
    })
}

impl<B, D, const EVENTS: usize> RadioController<B, D, EVENTS> {
    /// Split exclusive ownership into Wi-Fi control/data planes and the runner.
    pub fn split(self) -> RadioParts<B, D, EVENTS> {
        RadioParts {
            wifi: WifiParts {
                controller: WifiController {
                    state: self.state,
                    next_sequence: 0,
                },
                device: WifiDevice {
                    inner: self.resources.device,
                },
            },
            runner: RadioRunner {
                backend: self.resources.backend,
                config: self.config.wifi,
                state: self.state,
                last_poll_error: None,
            },
        }
    }
}

/// Enabled protocol handles plus the mandatory runner.
pub struct RadioParts<B, D, const EVENTS: usize> {
    /// Wi-Fi control and L2 data planes.
    pub wifi: WifiParts<D, EVENTS>,
    /// Long-lived backend runner.
    pub runner: RadioRunner<B, EVENTS>,
}

/// Separate Wi-Fi control and L2 data-plane ownership.
pub struct WifiParts<D, const EVENTS: usize> {
    /// Async control plane.
    pub controller: WifiController<EVENTS>,
    /// L2 data plane.
    pub device: WifiDevice<D>,
}

/// Async Wi-Fi control plane. This handle is deliberately not cloneable.
pub struct WifiController<const EVENTS: usize> {
    state: &'static RadioState<EVENTS>,
    next_sequence: u32,
}

impl<const EVENTS: usize> WifiController<EVENTS> {
    /// Ask the runner to initialize the backend.
    ///
    /// Dropping this future stops waiting but does not cancel an operation the
    /// runner has already received. A later command ignores that stale result.
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let sequence = self.allocate_sequence();
        self.state
            .shared
            .commands
            .send(Command {
                sequence,
                kind: CommandKind::Initialize,
            })
            .await;
        loop {
            let completion = self.state.shared.completion.wait().await;
            if completion.sequence != sequence {
                continue;
            }
            return match completion.kind {
                CompletionKind::Initialize(result) => result.map_err(Error::Backend),
                _ => Err(Error::Protocol),
            };
        }
    }

    /// Scan and copy results into a caller-provided fixed buffer.
    pub async fn scan(
        &mut self,
        config: ScanConfig,
        output: &mut [ScanResult],
    ) -> Result<ScanOutcome, Error> {
        let sequence = self.allocate_sequence();
        self.state
            .shared
            .commands
            .send(Command {
                sequence,
                kind: CommandKind::Scan(config),
            })
            .await;
        loop {
            let completion = self.state.shared.completion.wait().await;
            if completion.sequence != sequence {
                continue;
            }
            return match completion.kind {
                CompletionKind::Scan(result) => {
                    let backend = result.map_err(Error::Backend)?;
                    let count = backend.count.min(output.len());
                    output[..count].copy_from_slice(&self.state.shared.scan_results()[..count]);
                    Ok(ScanOutcome {
                        count,
                        truncated: backend.truncated || backend.count > output.len(),
                    })
                }
                _ => Err(Error::Protocol),
            };
        }
    }

    /// Associate and authorize a station connection.
    pub async fn connect(&mut self, config: StationConfig) -> Result<ConnectionInfo, Error> {
        let sequence = self.allocate_sequence();
        self.state
            .shared
            .commands
            .send(Command {
                sequence,
                kind: CommandKind::Connect(config),
            })
            .await;
        loop {
            let completion = self.state.shared.completion.wait().await;
            if completion.sequence != sequence {
                continue;
            }
            return match completion.kind {
                CompletionKind::Connect(result) => result.map_err(Error::Backend),
                _ => Err(Error::Protocol),
            };
        }
    }

    /// Disconnect the current station link.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        let sequence = self.allocate_sequence();
        self.state
            .shared
            .commands
            .send(Command {
                sequence,
                kind: CommandKind::Disconnect,
            })
            .await;
        loop {
            let completion = self.state.shared.completion.wait().await;
            if completion.sequence != sequence {
                continue;
            }
            return match completion.kind {
                CompletionKind::Disconnect(result) => result.map_err(Error::Backend),
                _ => Err(Error::Protocol),
            };
        }
    }

    /// Wait for the next bounded event produced by the runner.
    pub async fn next_event(&mut self) -> WifiEvent {
        self.state.shared.events.receive().await
    }

    /// Snapshot queue occupancy and overflow.
    pub fn event_diagnostics(&self) -> EventDiagnostics {
        EventDiagnostics {
            capacity: EVENTS,
            pending: self.state.shared.events.len(),
            dropped: self.state.shared.dropped_events.load(Ordering::Relaxed),
        }
    }

    fn allocate_sequence(&mut self) -> u32 {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        if self.next_sequence == 0 {
            self.next_sequence = 1;
        }
        self.next_sequence
    }
}

/// Long-lived owner of a chip backend.
pub struct RadioRunner<B, const EVENTS: usize> {
    backend: B,
    config: WifiConfig,
    state: &'static RadioState<EVENTS>,
    last_poll_error: Option<BackendError>,
}

impl<B: WifiBackend, const EVENTS: usize> RadioRunner<B, EVENTS> {
    /// Process at most one command and one bounded background-work batch.
    ///
    /// A `true` result means another batch may be useful immediately; it does
    /// not grant the caller permission to monopolize a cooperative executor.
    /// A thread-based runner must yield or otherwise provide a scheduling point
    /// between calls.
    pub fn run_once(&mut self) -> bool {
        let mut did_work = false;
        if let Ok(command) = self.state.shared.commands.try_receive() {
            self.process_command(command);
            did_work = true;
        }
        match self.backend.poll() {
            Ok(background_work) => {
                self.last_poll_error = None;
                did_work || background_work
            }
            Err(error) => {
                if self.last_poll_error != Some(error) {
                    self.state.shared.publish_event(WifiEvent::Failed(error));
                    self.last_poll_error = Some(error);
                    true
                } else {
                    did_work
                }
            }
        }
    }

    /// Run forever for command-driven backends.
    ///
    /// Backends with timer- or RX-driven [`WifiBackend::poll`] work must call
    /// [`Self::run_once`] from their platform runner so its wait primitive can
    /// cover both command and backend wake sources.
    pub async fn run(mut self) -> ! {
        loop {
            let command = self.state.shared.commands.receive().await;
            self.process_command(command);
        }
    }

    fn process_command(&mut self, command: Command) {
        let sequence = command.sequence;
        let completion = match command.kind {
            CommandKind::Initialize => {
                let result = self.backend.initialize(&self.config);
                self.publish_result(result, WifiEvent::Initialized);
                CompletionKind::Initialize(result)
            }
            CommandKind::Scan(config) => {
                // SAFETY: RadioRunner is unique and processes one command at a
                // time. Completion is signalled only after this borrow ends.
                let output = unsafe { &mut *self.state.shared.scan_results_ptr() };
                let result = self.backend.scan(config, output);
                match result {
                    Ok(outcome) => self.state.shared.publish_event(WifiEvent::ScanCompleted {
                        count: outcome.count,
                        truncated: outcome.truncated,
                    }),
                    Err(error) => self.state.shared.publish_event(WifiEvent::Failed(error)),
                }
                CompletionKind::Scan(result)
            }
            CommandKind::Connect(config) => {
                let result = self.backend.connect(&config);
                match result {
                    Ok(info) => self.state.shared.publish_event(WifiEvent::Connected(info)),
                    Err(error) => self.state.shared.publish_event(WifiEvent::Failed(error)),
                }
                CompletionKind::Connect(result)
            }
            CommandKind::Disconnect => {
                let result = self.backend.disconnect(&self.config);
                self.publish_result(result, WifiEvent::Disconnected { reason: 0 });
                CompletionKind::Disconnect(result)
            }
        };
        self.state.shared.completion.signal(Completion {
            sequence,
            kind: completion,
        });
    }

    fn publish_result(&self, result: Result<(), BackendError>, success: WifiEvent) {
        self.state.shared.publish_event(match result {
            Ok(()) => success,
            Err(error) => WifiEvent::Failed(error),
        });
    }
}

/// L2 data-plane ownership independent of the control backend.
pub struct WifiDevice<D> {
    inner: D,
}

impl<D> WifiDevice<D> {
    /// Borrow the chip L2 device.
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Mutably borrow the chip L2 device.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    /// Recover the chip L2 device.
    pub fn into_inner(self) -> D {
        self.inner
    }
}

#[cfg(feature = "smoltcp")]
impl<D: smoltcp::phy::Device> smoltcp::phy::Device for WifiDevice<D> {
    type RxToken<'a>
        = D::RxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = D::TxToken<'a>
    where
        Self: 'a;

    fn receive(
        &mut self,
        timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(timestamp)
    }

    fn transmit(&mut self, timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(timestamp)
    }

    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        self.inner.capabilities()
    }
}

pub(crate) struct Command {
    sequence: u32,
    kind: CommandKind,
}

pub(crate) enum CommandKind {
    Initialize,
    Scan(ScanConfig),
    Connect(StationConfig),
    Disconnect,
}

#[derive(Clone, Copy)]
pub(crate) struct Completion {
    sequence: u32,
    kind: CompletionKind,
}

#[derive(Clone, Copy)]
pub(crate) enum CompletionKind {
    Initialize(Result<(), BackendError>),
    Scan(Result<ScanOutcome, BackendError>),
    Connect(Result<ConnectionInfo, BackendError>),
    Disconnect(Result<(), BackendError>),
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::future::Future;
    use core::task::{Context, Poll, Waker};
    use std::boxed::Box;

    use super::*;

    #[derive(Default)]
    struct MockBackend {
        calls: u8,
        poll_work: bool,
        poll_error: Option<BackendError>,
    }

    impl WifiBackend for MockBackend {
        fn initialize(&mut self, _: &WifiConfig) -> Result<(), BackendError> {
            self.calls += 1;
            Ok(())
        }

        fn scan(
            &mut self,
            _: ScanConfig,
            output: &mut [ScanResult],
        ) -> Result<ScanOutcome, BackendError> {
            self.calls += 1;
            output[0] = ScanResult {
                ssid: Ssid::try_from_bytes(b"test-ap").unwrap(),
                bssid: [1, 2, 3, 4, 5, 6],
                frequency_mhz: 2437,
                rssi_dbm: -42,
                security: Security::Wpa2Personal,
                channel: 6,
            };
            Ok(ScanOutcome {
                count: 1,
                truncated: false,
            })
        }

        fn connect(&mut self, config: &StationConfig) -> Result<ConnectionInfo, BackendError> {
            self.calls += 1;
            Ok(ConnectionInfo {
                bssid: config.bssid,
                frequency_mhz: 2437,
            })
        }

        fn disconnect(&mut self, _: &WifiConfig) -> Result<(), BackendError> {
            self.calls += 1;
            Ok(())
        }

        fn poll(&mut self) -> Result<bool, BackendError> {
            if let Some(error) = self.poll_error {
                Err(error)
            } else {
                Ok(core::mem::take(&mut self.poll_work))
            }
        }
    }

    fn poll<F: Future>(future: core::pin::Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::noop();
        future.poll(&mut Context::from_waker(waker))
    }

    #[test]
    fn runner_is_the_only_backend_execution_path() {
        let state = Box::leak(Box::new(RadioState::<4>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: MockBackend::default(),
                device: (),
            },
            state,
        )
        .unwrap();
        let RadioParts {
            mut wifi,
            mut runner,
        } = radio.split();

        {
            let mut initialize = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(initialize.as_mut()).is_pending());
            assert!(runner.run_once());
            assert_eq!(poll(initialize.as_mut()), Poll::Ready(Ok(())));
        }

        let mut results = [ScanResult::EMPTY; 1];
        {
            let mut scan = core::pin::pin!(wifi.controller.scan(
                ScanConfig::try_from_timeout_ms(1_000).unwrap(),
                &mut results,
            ));
            assert!(poll(scan.as_mut()).is_pending());
            assert!(runner.run_once());
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
    fn bounded_events_drop_the_oldest_and_report_overflow() {
        let state = Box::leak(Box::new(RadioState::<1>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: MockBackend::default(),
                device: (),
            },
            state,
        )
        .unwrap();
        let RadioParts {
            mut wifi,
            mut runner,
        } = radio.split();

        for _ in 0..2 {
            let mut initialize = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(initialize.as_mut()).is_pending());
            assert!(runner.run_once());
            assert_eq!(poll(initialize.as_mut()), Poll::Ready(Ok(())));
        }
        assert_eq!(
            wifi.controller.event_diagnostics(),
            EventDiagnostics {
                capacity: 1,
                pending: 1,
                dropped: 1,
            }
        );
    }

    #[test]
    fn runner_advances_background_work_without_a_command() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: MockBackend {
                    poll_work: true,
                    ..MockBackend::default()
                },
                device: (),
            },
            state,
        )
        .unwrap();
        let mut runner = radio.split().runner;

        assert!(runner.run_once());
        assert!(!runner.run_once());
    }

    #[test]
    fn repeated_background_error_publishes_one_event() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let error = BackendError {
            class: BackendErrorClass::Other,
            code: 0x55,
        };
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: MockBackend {
                    poll_error: Some(error),
                    ..MockBackend::default()
                },
                device: (),
            },
            state,
        )
        .unwrap();
        let RadioParts {
            mut wifi,
            mut runner,
        } = radio.split();

        assert!(runner.run_once());
        assert!(!runner.run_once());
        assert_eq!(
            wifi.controller.event_diagnostics(),
            EventDiagnostics {
                capacity: 2,
                pending: 1,
                dropped: 0,
            }
        );
        let mut event = core::pin::pin!(wifi.controller.next_event());
        assert_eq!(poll(event.as_mut()), Poll::Ready(WifiEvent::Failed(error)));
    }

    #[test]
    fn cancelled_control_future_cannot_poison_the_next_command() {
        let state = Box::leak(Box::new(RadioState::<2>::new()));
        let radio = init(
            RadioConfig::default(),
            RadioResources {
                backend: MockBackend::default(),
                device: (),
            },
            state,
        )
        .unwrap();
        let RadioParts {
            mut wifi,
            mut runner,
        } = radio.split();

        {
            let mut cancelled = core::pin::pin!(wifi.controller.initialize());
            assert!(poll(cancelled.as_mut()).is_pending());
        }
        assert!(runner.run_once());

        let mut next = core::pin::pin!(wifi.controller.initialize());
        assert!(poll(next.as_mut()).is_pending());
        assert!(runner.run_once());
        assert_eq!(poll(next.as_mut()), Poll::Ready(Ok(())));
    }

    #[test]
    fn validated_configuration_rejects_invalid_inputs() {
        assert!(Ssid::try_from_bytes(b"").is_none());
        assert!(Ssid::try_from_bytes(&[b'x'; 33]).is_none());
        assert!(Passphrase::try_from_ascii(b"short").is_none());
        assert!(Passphrase::try_from_ascii(b"testtest").is_some());
        assert!(ScanConfig::try_from_timeout_ms(0).is_none());
    }

    #[test]
    fn wpa3_config_requires_wpa3_scan_and_implies_required_pmf() {
        let result = ScanResult {
            ssid: Ssid::try_from_bytes(b"wpa3-ap").unwrap(),
            bssid: [1, 2, 3, 4, 5, 6],
            frequency_mhz: 5180,
            rssi_dbm: -38,
            security: Security::Wpa3Personal,
            channel: 36,
        };
        let config = StationConfig::wpa3_personal(
            &result,
            Passphrase::try_from_ascii(b"testtest").unwrap(),
            SaePwe::Both,
            10_000,
        )
        .unwrap();
        assert_eq!(
            config.security(),
            PersonalSecurity::Wpa3 {
                sae_pwe: SaePwe::Both
            }
        );
        assert_eq!(
            config.security().management_frame_protection(),
            ManagementFrameProtection::Required
        );
    }
}
