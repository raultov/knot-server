# Changelog

All notable changes to `knot-server` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

## [0.1.17] - 2026-06-06

### Added
- `up.sh` and `down.sh` convenience scripts for Docker Compose lifecycle management

### Changed
- Upgrade `knot` dependency from 1.3.11 to 1.3.13

### Fixed
- E2E tests: cache fastembed model to avoid Hugging Face 429 rate limits in CI

## [0.1.16] - 2026-06-01

### Changed
- Upgrade `knot` dependency from 1.3.10 to 1.3.11

### Fixed
- Graph viewer: reset overview defaults on repo switch, isolate clear button, focus depth=1

### Docs
- Update roadmap language support status, remove duplicate line

## [0.1.15] - 2026-05-31

### Added
- Graph viewer: improve color coding, auto-enable kind filters, save depth on focus
- Postman collection for API testing
- Beta disclaimer and roadmap to README

### Changed
- Upgrade `knot` dependency from 1.3.9 to 1.3.10

### Fixed
- Filter self-referencing edges from graph responses

## [0.1.14] - 2026-05-29

### Added
- utoipa Swagger UI and OpenAPI spec generation (`/docs` endpoint)
- Expose Neo4j (7474, 7687) and Qdrant (6333, 6334) ports in docker-compose

### Docs
- Document Swagger UI, update installer URL, add API workflow step

## [0.1.13] - 2026-05-28

### Fixed
- CI: downgrade actions/download-artifact from v7 to v4 and actions/checkout from v6 to v4

## [0.1.12] - 2026-05-28

### Changed
- Rename `RepoStatus::Idle` to `Indexed` and introduce `Pending` initial state

## [0.1.11] - 2026-05-28

### Added
- Display knot-server version in `/graph` UI via compile-time placeholder substitution

## [0.1.10] - 2026-05-28

### Added
- Make `KNOT_SERVER_PORT` configurable in docker-compose.yml

## [0.1.9] - 2026-05-27

### Changed
- Version bump (no functional changes)

## [0.1.8] - 2026-05-26

### Added
- Dev compose overlay (`docker-compose.dev.yml`) and entrypoint script
- OpenAPI docs endpoint

### Fixed
- CI: use correct variant for allow-dirty
- CI: trigger docker publish only on tags to avoid duplicates
- CI: allow dirty release workflow for manual edits
- CI: handle existing release in gh release step
- CI: prevent duplicate docker push on tag+master events

## [0.1.7] - 2026-05-24

### Added
- Enhanced graph viewer UX and connectivity
- Enable docker publish on master push

## [0.1.6] - 2026-05-23

### Added
- Enhanced graph overview with relationship toggles and FQN support

## [0.1.5] - 2026-05-15

### Added
- Make `batch_size` and `ingest_concurrency` configurable for performance tuning
- Cluster coordination E2E test

### Docs
- Add performance tuning section and network host guide

## [0.1.4] - 2026-05-10

### Fixed
- Fix glibc compatibility with ubuntu-24.04 custom runner

## [0.1.3] - 2026-05-10

### Added
- cargo-dist installer with auto OS/arch detection
- E2E test workflow

### Docs
- Single curl install command with auto OS/arch detection

## [0.1.2] - 2026-05-10

### Added
- crates.io metadata
- AI assistant skills (Cursor, Copilot, Claude, Gemini)
- Retry logic for transient failures
- Docker Compose hardening

## [0.1.0] - 2026-05-10

### Added
- Initial release: clustered indexing engine with distributed locking and webhooks
- Docker support with debian:trixie-slim builder for glibc 2.40+ compatibility
- CI/CD pipeline with Docker build/push and GitHub releases

---

[Unreleased]: https://github.com/raultov/knot-server/compare/v0.1.17...HEAD
[0.1.17]: https://github.com/raultov/knot-server/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/raultov/knot-server/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/raultov/knot-server/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/raultov/knot-server/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/raultov/knot-server/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/raultov/knot-server/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/raultov/knot-server/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/raultov/knot-server/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/raultov/knot-server/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/raultov/knot-server/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/raultov/knot-server/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/raultov/knot-server/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/raultov/knot-server/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/raultov/knot-server/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/raultov/knot-server/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/raultov/knot-server/compare/v0.1.0...v0.1.2
[0.1.0]: https://github.com/raultov/knot-server/releases/tag/v0.1.0
