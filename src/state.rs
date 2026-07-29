use core::cell::UnsafeCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

#[cfg(feature = "incremental-backend-experiment")]
use crate::IncrementalRunnerDiagnostics;
use crate::wifi::{
    Command, Completion, MAX_SCAN_RESULTS, ScanResult, WifiEvent, WifiL2Capabilities,
};

#[cfg(feature = "incremental-backend-experiment")]
const INCREMENTAL_DIAGNOSTIC_COUNTERS: usize = 18;

#[cfg(feature = "incremental-backend-experiment")]
pub(crate) struct IncrementalDiagnosticsState {
    counters: [AtomicU32; INCREMENTAL_DIAGNOSTIC_COUNTERS],
}

#[cfg(feature = "incremental-backend-experiment")]
impl IncrementalDiagnosticsState {
    const fn new() -> Self {
        Self {
            counters: [const { AtomicU32::new(0) }; INCREMENTAL_DIAGNOSTIC_COUNTERS],
        }
    }

    pub(crate) fn publish(&self, value: IncrementalRunnerDiagnostics) {
        for (counter, value) in self.counters.iter().zip(value.as_array()) {
            counter.store(value, Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self) -> IncrementalRunnerDiagnostics {
        let mut values = [0; INCREMENTAL_DIAGNOSTIC_COUNTERS];
        for (value, counter) in values.iter_mut().zip(&self.counters) {
            *value = counter.load(Ordering::Relaxed);
        }
        IncrementalRunnerDiagnostics::from_array(values)
    }
}

pub(crate) struct L2CapabilityState {
    valid: AtomicBool,
    station_mac_address: [AtomicU8; 6],
}

impl L2CapabilityState {
    const fn new() -> Self {
        Self {
            valid: AtomicBool::new(false),
            station_mac_address: [
                AtomicU8::new(0),
                AtomicU8::new(0),
                AtomicU8::new(0),
                AtomicU8::new(0),
                AtomicU8::new(0),
                AtomicU8::new(0),
            ],
        }
    }

    pub(crate) fn publish_once(&self, capabilities: WifiL2Capabilities) {
        if self.valid.load(Ordering::Acquire) {
            return;
        }
        for (destination, byte) in self
            .station_mac_address
            .iter()
            .zip(capabilities.station_mac_address())
        {
            destination.store(byte, Ordering::Relaxed);
        }
        self.valid.store(true, Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> Option<WifiL2Capabilities> {
        if !self.valid.load(Ordering::Acquire) {
            return None;
        }
        let mut station_mac_address = [0; 6];
        for (destination, source) in station_mac_address
            .iter_mut()
            .zip(&self.station_mac_address)
        {
            *destination = source.load(Ordering::Relaxed);
        }
        WifiL2Capabilities::try_new(station_mac_address)
    }
}

pub(crate) struct SharedState<const EVENTS: usize> {
    claimed: AtomicBool,
    pub(crate) commands: Channel<CriticalSectionRawMutex, Command, 1>,
    pub(crate) cancellations: Channel<CriticalSectionRawMutex, u32, 3>,
    pub(crate) completion: Signal<CriticalSectionRawMutex, Completion>,
    pub(crate) events: Channel<CriticalSectionRawMutex, WifiEvent, EVENTS>,
    pub(crate) dropped_events: AtomicU32,
    pub(crate) event_high_water: AtomicU32,
    pub(crate) command_high_water: AtomicU32,
    pub(crate) run_once_calls: AtomicU32,
    pub(crate) commands_processed: AtomicU32,
    pub(crate) backend_poll_calls: AtomicU32,
    pub(crate) backend_poll_work_batches: AtomicU32,
    pub(crate) backend_poll_errors: AtomicU32,
    pub(crate) immediate_repoll_hints: AtomicU32,
    #[cfg(feature = "incremental-backend-experiment")]
    pub(crate) incremental_diagnostics: IncrementalDiagnosticsState,
    pub(crate) l2_capabilities: L2CapabilityState,
    scan_results: UnsafeCell<[ScanResult; MAX_SCAN_RESULTS]>,
}

// SAFETY: `scan_results` has a single writer (the unique RadioRunner) and a
// single reader (the unique WifiController). The runner signals completion
// only after writing, and the controller cannot issue a second command while
// borrowing the previous output buffer. All other fields provide their own
// synchronization.
unsafe impl<const EVENTS: usize> Sync for SharedState<EVENTS> {}

impl<const EVENTS: usize> SharedState<EVENTS> {
    pub(crate) const fn new() -> Self {
        assert!(EVENTS > 0, "radio event queue must not be empty");
        Self {
            claimed: AtomicBool::new(false),
            commands: Channel::new(),
            cancellations: Channel::new(),
            completion: Signal::new(),
            events: Channel::new(),
            dropped_events: AtomicU32::new(0),
            event_high_water: AtomicU32::new(0),
            command_high_water: AtomicU32::new(0),
            run_once_calls: AtomicU32::new(0),
            commands_processed: AtomicU32::new(0),
            backend_poll_calls: AtomicU32::new(0),
            backend_poll_work_batches: AtomicU32::new(0),
            backend_poll_errors: AtomicU32::new(0),
            immediate_repoll_hints: AtomicU32::new(0),
            #[cfg(feature = "incremental-backend-experiment")]
            incremental_diagnostics: IncrementalDiagnosticsState::new(),
            l2_capabilities: L2CapabilityState::new(),
            scan_results: UnsafeCell::new([ScanResult::EMPTY; MAX_SCAN_RESULTS]),
        }
    }

    pub(crate) fn claim(&self) -> bool {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn scan_results_ptr(&self) -> *mut [ScanResult; MAX_SCAN_RESULTS] {
        self.scan_results.get()
    }

    pub(crate) fn scan_results(&self) -> &[ScanResult; MAX_SCAN_RESULTS] {
        // SAFETY: only the unique WifiController calls this after the runner's
        // completion signal established that the write has finished.
        unsafe { &*self.scan_results.get() }
    }

    pub(crate) fn publish_event(&self, event: WifiEvent) {
        if self.events.try_send(event).is_ok() {
            self.record_event_depth();
            return;
        }
        let _ = self.events.try_receive();
        saturating_increment(&self.dropped_events);
        let _ = self.events.try_send(event);
        self.record_event_depth();
    }

    fn record_event_depth(&self) {
        let depth = u32::try_from(self.events.len()).unwrap_or(u32::MAX);
        self.event_high_water.fetch_max(depth, Ordering::Relaxed);
    }

    pub(crate) fn record_command_accepted(&self) {
        // The command channel has capacity one. A completed send proves that
        // its slot was occupied even if the runner receives it immediately.
        self.command_high_water.fetch_max(1, Ordering::Relaxed);
    }
}

pub(crate) fn saturating_increment(counter: &AtomicU32) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}
