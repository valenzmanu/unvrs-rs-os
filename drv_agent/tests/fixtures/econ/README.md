# Recorded harness catalog evidence

Captured on 2026-10-01 with `cargo run -p drv_agent --example catalog_evidence -- <sandbox>` through tracked child processes. No user prompt or inference request was sent. Codex evidence comes from app-server `model/list`, `account/read`, `account/rateLimits/read` and the resulting local `models_cache.json`. Claude evidence comes from SDK `list_models`, `get_usage` and `auth status --json`.

Emails, account identifiers and reset-credit identifiers are redacted. Claude auth is restricted to sign-in/method/subscription fields. The models-cache fixture retains only its timestamp, model IDs and supported efforts. The Claude usage fixture retains only subscription and quota fields. No credentials or access/refresh tokens are included.

These fixtures establish parsing behavior at the captured harness versions; they are not evidence of current quota or authentication. Tests mutate copies to exercise exhaustion, unknowns, unsupported effort and missing models. Catalog discovery always refreshes live evidence before admission.
