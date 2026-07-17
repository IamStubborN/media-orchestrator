# TMDB Trending Design

## Goal

Add a shared media command that shows what is trending globally on TMDB, matching
the useful `/trending` behavior from the legacy `movie-tracker` without keeping
that service as a runtime dependency.

## User Experience

Hermes recognizes `/trending` and natural-language requests such as "what are
people watching", "popular movies", and "popular series". The default request
returns the first five weekly trending movies and series. Users can restrict the
category to movies or series and request another page through natural language
or an explicit page number.

Each item contains its position, localized title, original title when different,
year, media type, and TMDB rating when available. The response also identifies
the weekly TMDB source and the current page. It does not start a download. A
follow-up such as "find the second one on Rezka" starts a separate provider
search using the selected title.

## Architecture

`media-integrations` owns a typed TMDB HTTP client. `media-api` exposes a
protected read-only endpoint backed by a small trending service interface.
`media` composes the client, exposes the CLI command, and renders JSON for the
Hermes wrapper. No database state is required.

The flow is:

```text
Hermes -> hermes-media trending -> media-service -> TMDB
```

The API accepts:

- `category`: `all`, `movie`, or `tv`; default `all`.
- `page`: a positive TMDB page number; default `1`.
- `window`: fixed to `week` for this feature.

The service returns at most five results from the requested TMDB page plus TMDB
pagination metadata. Results keep their TMDB ordering.

## Configuration

`media-service` receives `MEDIA_TMDB_API_KEY` and
`MEDIA_TMDB_LANGUAGE` (default `ru`). The homelab deployment reuses the existing
TMDB credential value already configured for `movie-tracker`; the credential is
not returned by the API or exposed to Hermes.

TMDB configuration is optional at process startup so unrelated media operations
remain available. Calling the trending endpoint without a configured client
returns a clear unavailable error.

## Failure Handling

- Invalid category or page values return a validation error.
- TMDB authentication failures are reported as integration authentication
  errors without exposing credentials.
- Timeouts and malformed provider responses return a stable infrastructure
  error suitable for a concise Telegram response.
- An empty result page is successful and explicitly reports no titles.

## Hermes Integration

The constrained wrapper allows only:

```text
hermes-media trending [--category all|movie|tv] [--page N] --json
```

The shared media skill instructs both profiles to use this command instead of
web search. It keeps the last category and page in conversational context so
"show more" requests the next page. It presents no more than five items and
does not claim that TMDB popularity represents local Plex activity.

## Verification

Tests cover TMDB response mapping and errors, API authorization and validation,
CLI argument forwarding and rendering, wrapper allowlisting, and skill contract
text. A live smoke test calls the deployed command through `hermes-primary` and
confirms that a second-page request returns a different page.

