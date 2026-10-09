# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/devrandom-labs/bombay-timers/compare/bombay-timers-v0.1.0...bombay-timers-v0.2.0) - 2026-10-09

### Changed

- `TimerQueue::schedule` now returns `Result<Token<K>, ScheduleError<I, K, V>>`.
  Handle generation or sequence exhaustion explicitly. Rejection returns the
  original key, deadline and payload and preserves every existing schedule.

## [0.1.0](https://github.com/devrandom-labs/bombay-timers/releases/tag/bombay-timers-v0.1.0) - 2026-08-06

### Other

- prepare bombay-timers for publication
- establish timer research baseline
