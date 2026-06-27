# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - Draft

### Added

- Added DuckDNS provider support
- Added Porkbun provider support
- Added HTTP retry support for IP lookups and DNS provider updates
- Added configurable HTTP request and connect timeouts
- Added `ident.me` as a default HTTP IP lookup endpoint
- Added HTTP lookup quorum support before DNS provider updates

### Changed

- Switched the client cache from in-memory state to a durable MessagePack-backed file cache
- Hardened cache reads and writes with size checks, reserved flag validation, CRC validation ordering, and directory fsyncs
- Hardened HTTP IP lookups by rejecting non-2xx responses and capping response body reads
- Updated Cloudflare, DuckDNS, and Porkbun provider response handling to fail on HTTP errors
- Added startup provider config validation and improved config parse errors
- Updated client startup and loop handling to convert panics into errors and skip missed ticks
- Updated Rust and Docker toolchains and dependencies

### Fixed

- Fixed HTTP IP lookup fallback behavior when a lookup URL fails
- Fixed dual-stack cache handling to avoid overwriting one IP version with the other
- Avoided immediate startup retry bursts with a short startup delay

## [0.6.0] - 2025-07-19

### Added

- Added aggressive connect timeout to allow quicker fallbacks on provider lookups

### Changed

- Added secrecy crate to protect provider api keys
- Removed unused Serialize impls from config structs

## [0.5.0] - 2025-07-19

### Changed

- Removed the usage of STUN for IP lookups, now using HTTP(S) requests directly

## [0.4.0] - 2025-07-16

  ### Changed

- Updated STUN implementation to send/receive messages on the UDP socket directly
now that the stun v0.8.0 client is no longer `Sync`.
- Updated `IpUpdate` to use `Ipv4Addr` and `Ipv6Addr` instead of `IpAddr` to
  avoid unnecessary conversions.
- Updated Rust edition to 2024

## [0.3.0] - 2024-12-16

### Fixed

- Fixed a bug where the Cloudflare provider was given an incorrect domain name for record update/create

## [0.2.0] - 2024-12-16

### Added

- Added additional context to IP fetch errors

### Changed

- Updated to rust 1.83.0
- Updated default ttl, proxied, and comment values for Cloudflare
- Refactored Cloudflare provider update operations into separate functions

### Fixed

- Fixed a bug where an empty update could make it though to provider updates
- Fixed pluralized domain struct

## [0.1.0] - 2024-11-09

### Added

- Initial release of DDRS
