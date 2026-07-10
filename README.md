# media-orchestrator

Private Rust media orchestration for Hermes, Rezka, Prowlarr, qBittorrent, and Plex.

Project documentation:

- `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`
  is the canonical product design.
- `docs/ARCHITECTURE.md` defines Rust boundaries and dependency direction.

This repository will contain:

- The first-party Rust Rezka client.
- `media-service`, `download-runner`, and the `media` CLI.
- PostgreSQL entities and explicit SeaORM migrations.
- Docker images, API contracts, and tests.

Hermes profiles and skills belong in the separate private `hermes-home` repository. Homelab deployment wiring belongs in the existing `homelab` repository.

Implementation must not begin until the design is reviewed and approved.
