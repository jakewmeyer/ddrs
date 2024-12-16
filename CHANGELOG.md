# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
