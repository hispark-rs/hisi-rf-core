# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/hispark-rs/hisi-rf-core/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/hispark-rs/hisi-rf-core/releases/tag/v0.1.0-alpha.1
