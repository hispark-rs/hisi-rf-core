# hisi-rf-core

`hisi-rf-core` owns the chip-neutral radio contracts for the hispark-rs
ecosystem. It provides Wi-Fi control, L2 device ownership, a mandatory
background runner, and bounded events without owning an IP stack. Applications
normally select a chip through the `hisi-rf` facade; backend crates implement
the contracts defined here.

Chip repositories implement `WifiBackend`; applications drive TCP/IP through
`embassy-net` or the optional `smoltcp::phy::Device` adapter. Vendor archives,
ROM symbols, schedulers, TLS, NVS formats, and image packaging stay outside this
crate.

Public errors expose an allocation-free, versioned diagnostic view with stable
machine codes and recovery actions. The diagnostic schema deliberately excludes
SSID, passphrase, key material, and arbitrary backend text while preserving a
lossless numeric backend code.

This crate has no PAC, radio blob, scheduler, allocator, ROM, NVS, TLS, or image
format dependency. It is an early alpha; the public surface may change while
the single-dependency facade and WS63 backend are stabilized.
