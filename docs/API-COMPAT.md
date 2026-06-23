# API compatibility

NightKnight speaks three dialects over one store.

## v1 — legacy Nightscout

For existing uploaders and follower apps. Bare JSON arrays/objects, `.json` suffix
accepted.

| Method & path | Purpose |
|---|---|
| `GET /api/v1/entries[.json]` | recent entries (`count`, `find[date][$gte\|$lte]`, `find[dateString][...]`) |
| `GET /api/v1/entries/sgv[.json]` | sgv entries only |
| `GET /api/v1/entries/current[.json]` | latest sgv |
| `POST /api/v1/entries` | one entry or an array |
| `GET\|POST /api/v1/treatments` | treatments |
| `GET\|POST /api/v1/devicestatus` | device status |
| `GET\|POST /api/v1/profile` | profiles |
| `GET /api/v1/status[.json]` | status incl. `settings.units` |

## v3 — generic CRUD

`{ status, result }` envelope; content-derived deduplication; soft-delete; history.

| Method & path | Purpose |
|---|---|
| `GET /api/v3/{collection}` | search (`limit`, `date$gte`, `date$lte`, `type`) |
| `POST /api/v3/{collection}` | create (dedup → update on identifier match) |
| `GET\|PUT\|PATCH\|DELETE /api/v3/{collection}/{identifier}` | read / replace / merge / soft-delete |
| `GET /api/v3/{collection}/history[/{lastModifiedMs}]` | incremental changes (incl. deletions) |
| `GET /api/v3/lastModified` | latest change per collection |
| `GET /api/v3/status` | version + granted permissions |
| `GET /api/v3/version` | version (no auth) |

Collections: `entries`, `treatments`, `devicestatus`, `profile`, `food`, `settings`.

## v4 — modern (first-party default)

| Method & path | Purpose |
|---|---|
| `GET /api/v4/status` | service + the caller's user/unit |
| `GET /api/v4/current` | latest reading + trend, **in both units** |
| `GET /api/v4/entries?hours=&count=` | recent readings (mg/dL + mmol/L per point) |
| `GET /api/v4/analytics?hours=` | Time-in-Range, GMI, est. A1c, CV |
| `GET\|PUT /api/v4/me` | profile (preferred unit, display name) |
| `GET\|POST /api/v4/tokens`, `DELETE /api/v4/tokens/{id}` | device tokens |

## Authentication — the deliberate differences

- **Header-only credentials.** Tokens are read from `Authorization: Bearer <token>`
  or the Nightscout `api-secret` header. The legacy `?token=` / `?secret=` query
  parameters are **rejected** — query strings leak into logs, history and referrers.
  Some very old uploaders that only put the secret in the URL will not authenticate;
  use a client that sends headers.
- **Token forms accepted.** Modern clients send the raw token (Bearer or
  `api-secret`). Legacy Nightscout uploaders (e.g. xDrip+) that SHA-1-hash the secret
  and send the hex in `api-secret` also work — both resolve to the same token.
- **Scopes.** Device tokens carry `{api}:{collection}:{action}` scopes (e.g.
  `api:entries:create`). Write implies read on the same collection. Signed-in humans
  own their data and have full access to it.
- **Multi-user.** Every record is owned by a user resolved from the verified
  identity; no cross-user access is possible.

## Units

`entries`/`treatments` may carry `units` (`mg/dl` or `mmol/l`); a value's unit is
preserved, and the canonical mg/dL is used for all maths. Conversion uses
`mg/dL = mmol/L × 18.0156`. Legacy Nightscout used looser factors (18 or 18.0182) in
places; NightKnight standardises on 18.0156 (glucose molar mass 180.156 g/mol), which
reproduces the canonical clinical pairs exactly (70↔3.9, 180↔10.0, 54↔3.0, 250↔13.9).
