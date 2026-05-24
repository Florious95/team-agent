from __future__ import annotations

import re


AUTH_MODES = {"subscription", "official_api", "compatible_api"}
PROFILE_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
SECRET_KEYS = {"API_KEY", "AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY", "GEMINI_API_KEY"}
PROXY_ENV_KEYS = ("HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "https_proxy", "http_proxy", "all_proxy", "NO_PROXY", "no_proxy")
CA_ENV_KEYS = ("NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "REQUESTS_CA_BUNDLE")
COMPATIBLE_API_NETWORK_ENV_KEYS = PROXY_ENV_KEYS + CA_ENV_KEYS
PROFILE_SECRET_BOUNDARY_TEXT = """# Team Agent Profile Secret Boundary

Do not read, print, grep, cat, sed, copy, summarize, or open raw `*.env` files in this directory.
These files may contain API keys or auth tokens and must stay out of agent context.

Use `team-agent profile show <name> --workspace . --json` or `team-agent profile doctor <name> --workspace . --json`
for redacted status. If a required value is missing, ask the human user to edit the local profile file.
"""
