# Kernel settings API

The settings registry belongs to `uke`. Clients render its schema instead of keeping a second list of settings. The fixture at `observatory/fixtures/settings.json` is a representative response, not a snapshot of an installed kernel. The implementation must generate the complete registry from the kernel's files, defaults and environment.

## Read contract

`GET /api/settings`, kernel operation `{"op":"settings.get"}`, and `unvrs settings get` return the same JSON object:

```json
{
  "schema": [],
  "values": {},
  "effective_source": {},
  "live_catalog": {},
  "history": []
}
```

`schema` is an ordered array. Each entry has these fields:

| Field | Meaning |
|---|---|
| `key` | Stable setting key, such as `econ.reasoning.deep`. |
| `section` | `routing`, `sources`, `projects`, `safety`, `kernel`, or `overrides`. |
| `label` | Plain words for the setting name. |
| `explanation` | What it changes, including an example. |
| `type` | `enum`, `int`, `bool`, `string`, `path`, `list`, `ordered-list`, or `table`. |
| `default` | JSON default, or `null` when no default exists. |
| `allowed` | Allowed JSON values, or `null` when unrestricted. |
| `range` | Inclusive `{ "min": n, "max": n }`, or `null`. |
| `file` | File path relative to UNVRS home, or `null` for environment/build settings. |
| `toml_path` | Array of TOML field names and zero-based array indices, or `null`. |
| `apply` | `live` or `restart`. Live applies to subsequent operations, not running tasks. |
| `editable` | Only routing and sources are editable in version 1. |
| `sensitive` | Changing this value can weaken safeguards; requires explicit confirmation. |
| `fields` | Object-row field descriptions for lists/tables, or `null`. Descriptors contain `type`, `explanation`, `allowed`, `required`; `allowed` may be `null`. |

`values[key]` is the current effective JSON value. `effective_source[key]` is `default`, `file`, or `env`. Environment overrides appear as their own read-only entries, e.g. `overrides.UNVRS_L3_CODEX_MODEL`, with an explanation that a model override “bypasses your routing rules”. An unset override has a `null` value and source `default`; the reader also includes detected `UNVRS_*` overrides. Project keys use `projects.<project-id>.<field>`; source keys use `sources.<source-id>.<field>`. An explicit TOML path, rather than splitting the key, identifies the underlying field.

Rules and profile candidates are edited as complete ordered arrays. Their row fields explain every nested TOML property. Source records expose every field separately, including `branch` and `refresh`. The schema includes the implicit memory source; a write materializes it in `sources.toml`. Read-only project fields and kernel constants each have explanations. `live_catalog` is the current econ harness-catalog map (models, efforts, evidence timestamps, auth/quota availability and errors); it is never editable through this API.

`history` is a newest-first array of successful changes:

```json
{
  "change_id": "settings-1790890000000-1",
  "key": "econ.reasoning.deep",
  "before": "high",
  "after": "medium",
  "by": "captain",
  "at": 1790890000000,
  "reverts": null
}
```

`at` is Unix time in milliseconds; `by` identifies the socket caller or the authenticated Observatory. `reverts` names the original change when this record is a revert. Journal event `config.changed` includes these fields. `settings.history` returns `{ "history": [...] }`.

## Write contract

`POST /api/settings` accepts one of:

```json
{"op":"settings.set","key":"econ.reasoning.deep","value":"medium","confirm":false}
{"op":"settings.revert","change_id":"settings-1790890000000-1","confirm":false}
```

The Unix socket accepts the same request objects. Successful writes return `{ "ok": true, "change": <history-record> }`. Clients then fetch settings again to show the validated effective values. Revert restores only the original setting, preserves other settings, validates against the current registry and refuses when that setting no longer equals the recorded `after` value. Revert records a new change and follows the same sensitive-setting confirmation rule.

Invalid JSON/types, out-of-range or unsupported values, unknown keys, read-only settings and invalid whole-file combinations are refused before any write. Sensitive changes require `confirm: true`. Files are edited with `toml_edit`, preserving comments and order of untouched items, backed up, atomically replaced with a same-directory temporary file and reloaded. Econ routing already reads configuration for each decision.

HTTP errors return `{ "ok": false, "error": "<plain explanation>" }`; status is 400 for validation, 403 for authentication/origin/CSRF refusal, 409 for a conflicting revert, and 500 for persistence failure. The CLI prints plain errors and exits unsuccessfully.

## Browser authentication

Writes require all of:

- `Host: unvrs.localhost:7576`.
- `Origin: http://unvrs.localhost:7576` (missing, null, other schemes, aliases and ports are refused).
- `X-UNVRS-CSRF: <page-token>`.
- `Content-Type: application/json`.

The kernel persists a random local authorization token at `kernel/settings.token` with mode `0600`. This token stays server-side: it is never returned by GET, embedded in HTML, sent by the browser, or logged. After the Observatory validates the exact Host/Origin and the page CSRF value, it uses the local token to authorize the kernel write internally. JSON bodies cannot claim this authority or select the recorded caller.

The server embeds only a page-scoped CSRF value in Observatory HTML served at the exact write origin as `<meta name="unvrs-settings-csrf" content="...">`. CSRF values are random and expire when the server restarts. The server retains the latest 1024 page values; an older page must reload after that limit is reached. They do not appear in `/api/settings`, fixtures or URLs. Pages served under read aliases receive no CSRF value. Existing read routes retain their behavior and no CORS permission is added. The browser sends no bearer header and there is no `unvrs-settings-token` meta element.

```js
const csrf = document.querySelector('meta[name="unvrs-settings-csrf"]')?.content;
await fetch('/api/settings', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'X-UNVRS-CSRF': csrf,
  },
  body: JSON.stringify({op: 'settings.set', key, value, confirm}),
});
```

## CLI

```text
unvrs settings                    # same as get
unvrs settings get
unvrs settings set <key> <json-value> [--confirm]
unvrs settings history
unvrs settings revert <change-id> [--confirm]
```

Use JSON strings for string/enum values, e.g. `unvrs settings set econ.reasoning.deep '"medium"'`. Tables and lists use JSON objects/arrays. CLI writes use the existing Unix socket identity boundary; HTTP credentials are not required on the socket. Worker requests must not gain captain configuration authority.
