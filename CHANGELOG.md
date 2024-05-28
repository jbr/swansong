# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/jbr/swansong/compare/v0.3.0...v0.3.1) - 2024-05-28

### Other
- *(deps)* update codecov/codecov-action action to v4.4.1
- *(deps)* update rust crate async-executor to v1.12.0
- *(deps)* bump taiki-e/cache-cargo-install-action from 1 to 2
- *(deps)* update rust crate async-executor to 1.11.0
- *(deps)* bump codecov/codecov-action from 4.2.0 to 4.3.0
- *(deps)* bump async-executor from 1.9.1 to 1.10.0

## [0.3.0](https://github.com/jbr/swansong/compare/v0.2.0...v0.3.0) - 2024-04-07

### Added
- add eq and partial eq implementations
- add Interrupt::into_inner
- add predicates on ShutdownState
- [**breaking**] make futures-io optional, add tokio feature
- [**breaking**] another iteration on the public interface and docs

### Fixed
- *(deps)* update rust crate event-listener to 5.3.0
- make timeout slower for windows

### Other
- test size hints for stream and iterator wrappers
- test eq impls
- attempt to resolve intermittent test failures
- try to catch random seed for flaky test
- fix references to old api
- *(actions)* document deps
- *(deps)* update codecov/codecov-action action to v4.2.0
- Add renovate.json
- *(deps)* bump async-executor from 1.8.0 to 1.9.1
- *(deps)* bump pin-project-lite from 0.2.13 to 0.2.14
- *(deps)* bump actions/configure-pages from 4 to 5
- *(deps)* bump codecov/codecov-action from 4.0.1 to 4.1.1
- Update README.md

## [0.2.0](https://github.com/jbr/swansong/compare/v0.1.0...v0.2.0) - 2024-04-01

### Added
- [**breaking**] reimplement swansong

### Other
- *(actions)* run tests with trace log level
- update readme
- Add coverage to readme
- *(actions)* add conventional commit check
- *(deps)* bump fastrand from 2.0.1 to 2.0.2
- *(actions)* ignore miri for now, add coverage
- *(deps)* bump futures-lite from 2.2.0 to 2.3.0

## [0.1.0](https://github.com/jbr/swansong/releases/tag/v0.1.0) - 2024-03-08

### Other
- *(deps)* bump actions/checkout from 3 to 4
- *(deps)* bump stopper from 0.2.5 to 0.2.6
- 🦢
