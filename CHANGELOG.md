# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/hispark-rs/hisi-rf-core/compare/v0.1.0-alpha.4...HEAD
[0.1.0-alpha.4]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.4
[0.1.0-alpha.3]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.1
