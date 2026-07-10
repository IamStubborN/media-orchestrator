# media-orchestrator

Private Rust media orchestration for Hermes, Rezka, Prowlarr, qBittorrent, and Plex.

Project documentation:

- `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`
  is the canonical product design.
- `docs/ARCHITECTURE.md` defines Rust boundaries and dependency direction.
- `docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md`
  defines the MVP delivery sequence.
- `docs/superpowers/plans/2026-07-10-rust-domain-foundation.md`
  is the first executable implementation plan.

This repository will contain:

- The first-party Rust Rezka client.
- `media-service`, `download-runner`, and the `media` CLI.
- PostgreSQL entities and explicit SeaORM migrations.
- Docker images, API contracts, and tests.

Hermes profiles and skills belong in the separate private `hermes-home` repository. Homelab deployment wiring belongs in the existing `homelab` repository.

## Development

The repository uses `mise` as its only supported developer entry point:

```bash
mise trust
mise install
mise run format
mise run check
mise run lint
mise run test
mise run audit
```

The current Rust foundation contains pure domain types, versioned transport
DTOs, the initial `media` composition binary, and executable architecture
checks. Network providers, PostgreSQL, filesystem access, ffmpeg, and container
runtime behavior are intentionally introduced by later focused plans in the
[MVP roadmap](docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md).
