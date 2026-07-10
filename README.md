# media-orchestrator

Private Rust media orchestration for Hermes, Rezka, Prowlarr, qBittorrent, and Plex.

This repository will contain:

- The first-party Rust Rezka client.
- `media-service`, `download-runner`, and the `media` CLI.
- PostgreSQL entities and explicit SeaORM migrations.
- Docker images, API contracts, and tests.

The product design is documented in [`docs/DESIGN.md`](docs/DESIGN.md). The
crate map and dependency rules are documented in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

Hermes profiles and skills belong in the separate private `hermes-home` repository. Homelab deployment wiring belongs in the existing `homelab` repository.

Implementation must not begin until the design is reviewed and approved.
