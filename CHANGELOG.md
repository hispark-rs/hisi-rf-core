# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added an executor-neutral incremental wait-platform contract and
  `IncrementalRadioRunner::wait_ready`, which registers command and platform
  wake sources together without consuming commands or busy polling.

## [0.1.0-alpha.10] - 2026-07-23

### Added

- Added an atomic incremental wait-intent snapshot and a non-consuming command
  wait future so platform adapters can compose command, backend, L2, and timer
  wakes without busy polling or guessing the backend deadline.

## [0.1.0-alpha.9] - 2026-07-23

### Added

- Added an opt-in `split_incremental` facade adapter that preserves the existing
  async `WifiController`, L2 device, bounded event queue, and scan storage while
  advancing one driver action per explicit platform wait-set wake.

## [0.1.0-alpha.8] - 2026-07-23

### Fixed

- Added explicit terminal transitions for backend start, poll, and cancellation
  errors so a failed experimental operation cannot retain the only runner slot.
  The transition preserves whether cancellation was already pending.

### Added

- Added a bounded active/pending command arbiter for the future incremental
  runner. Replacement commands request cancellation exactly once, queue overflow
  returns ownership to the caller, and stale operation generations fail closed.
- Added an executable opt-in backend driver that composes command arbitration,
  operation generations, fixed work budgets, wake selection, cancellation, and
  terminal slot recovery without changing the default blocking runner.

## [0.1.0-alpha.7] - 2026-07-23

### Added

- Added a deterministic incremental runner state machine with fair command,
  backend, L2 RX, and timer wake selection.
- Bound every work report to an operation generation and defined queued/start,
  cancel-before-start, cancel-after-start, stale completion, and budget-exhausted
  transitions.

### Changed

- Tightened the experimental backend poll contract with an explicit wake reason,
  next deadline, progress accounting, and fail-closed budget validation. The
  validated blocking backend remains unchanged and is still the default.

## [0.1.0-alpha.6] - 2026-07-23

### Added

- Added the opt-in `incremental-backend-experiment` contract with generation-tagged
  operation identities, explicit cancellation lifecycle, bounded work reports,
  and a composable runner wait set. The existing blocking backend remains the
  default.

## [0.1.0-alpha.5] - 2026-07-23

### Added

- Distinct `vendor_status` and `hostap_status` numeric trace fields so
  chip-driver, IEEE 802.11, and upstream supplicant failures remain
  machine-distinguishable without carrying arbitrary text or secrets.

## [0.1.0-alpha.4] - 2026-07-23

### Added

- Stable `operation.cancelled` and `resource.unavailable` diagnostic classes.
- Bounded `resource_required` and `resource_available` trace fields for
  initialization admission failures.
- A fixture matrix covering association rejection, first-EAPOL timeout,
  cancellation, resource exhaustion, and runtime timeout without carrying
  secrets or arbitrary backend strings.

## [0.1.0-alpha.3] - 2026-07-22

### Added

- Allocation-free `hisi-rf-error/v2` diagnostics with explicit
  scan/authenticate/associate/SAE/EAPOL/PMF/disconnect/runtime stages.
- A four-entry numeric backend trace and immutable profile revision, with
  deterministic JSON escaping and secret-redaction coverage.

### Changed

- `BackendError` now uses constructors and accessors so chip backends attach
  stage/profile/trace context without exposing chip-private error enums.

## [0.1.0-alpha.2] - 2026-07-22

### Added

- Allocation-free `hisi-rf-error/v1` diagnostics with stable codes, operation
  stages, recovery actions, documentation anchors, and deterministic JSON.
- Lossless backend-code preservation without carrying SSIDs, passphrases, key
  material, or arbitrary backend strings into public diagnostics.

## [0.1.0-alpha.1] - 2026-07-20

### Added

- Initial independent `hisi-rf-core` release, split mechanically from the
  chip-neutral implementation previously published as `hisi-rf`.
- `RadioController`, `RadioParts`, mandatory `RadioRunner`, typed Wi-Fi
  scan/station configuration, and bounded event delivery.
- Separate `WifiController` and L2 `WifiDevice`, with an optional
  `smoltcp::phy::Device` adapter.
- Typed WPA3-Personal station configuration with mandatory PMF and explicit SAE
  password-element policy.
- Explicit WPA2/WPA3-Personal transition scan classification; callers choose
  PSK or SAE instead of discovery silently downgrading to WPA2.

[Unreleased]: https://github.com/hispark-rs/hisi-rf-core/compare/v0.1.0-alpha.8...HEAD
[0.1.0-alpha.8]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.8
[0.1.0-alpha.7]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.7
[0.1.0-alpha.6]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.6
[0.1.0-alpha.5]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.5
[0.1.0-alpha.4]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.4
[0.1.0-alpha.3]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.1
