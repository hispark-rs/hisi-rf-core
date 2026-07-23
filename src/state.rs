use core::cell::UnsafeCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};

use crate::wifi::{Command, Completion, MAX_SCAN_RESULTS, ScanResult, WifiEvent};

pub(crate) struct SharedState<const EVENTS: usize> {
    claimed: AtomicBool,
    pub(crate) commands: Channel<CriticalSectionRawMutex, Command, 1>,
    pub(crate) completion: Signal<CriticalSectionRawMutex, Completion>,
    pub(crate) events: Channel<CriticalSectionRawMutex, WifiEvent, EVENTS>,
    pub(crate) dropped_events: AtomicU32,
    pub(crate) event_high_water: AtomicU32,
    pub(crate) run_once_calls: AtomicU32,
    pub(crate) commands_processed: AtomicU32,
    pub(crate) backend_poll_calls: AtomicU32,
    pub(crate) backend_poll_work_batches: AtomicU32,
    pub(crate) backend_poll_errors: AtomicU32,
    pub(crate) immediate_repoll_hints: AtomicU32,
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
            completion: Signal::new(),
            events: Channel::new(),
            dropped_events: AtomicU32::new(0),
            event_high_water: AtomicU32::new(0),
            run_once_calls: AtomicU32::new(0),
            commands_processed: AtomicU32::new(0),
            backend_poll_calls: AtomicU32::new(0),
            backend_poll_work_batches: AtomicU32::new(0),
            backend_poll_errors: AtomicU32::new(0),
            immediate_repoll_hints: AtomicU32::new(0),
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
}

pub(crate) fn saturating_increment(counter: &AtomicU32) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}
