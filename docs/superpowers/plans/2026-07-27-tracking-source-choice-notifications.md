# Tracking Source Choice Notifications Implementation Plan

1. Add source-choice domain and contract types with strict validation.
2. Add a storage migration for schema-v1 source-choice payloads and safe pending
   row conversion.
3. Change future-episode production and outbox rehydration to the typed payload.
4. Serialize source-choice deliveries through the Hermes integration.
5. Add source-choice parsing and rendering to the standalone notifier.
6. Add authorized Telegram callbacks for All sources, Rezka, and Prowlarr.
7. Preserve Telegram search scope through the hardened media wrapper.
8. Update the shared media skill and its assertions.
9. Run Rust and Python tests, formatting, linting, and legacy-producer scans.
10. Deploy both repositories, send a non-downloading source-choice probe, and
    verify the Telegram UI and callback behavior.
