from __future__ import annotations

from contextlib import contextmanager
import json
import os
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from team_agent.profiles.constants import COMPATIBLE_API_NETWORK_ENV_KEYS
from team_agent.profiles.provider_env import _compatible_api_network_exports, _profile_proxy_mode
from team_agent.rust_core import redact_text


def _anthropic_compatible_smoke(
    values: dict[str, str],
    model: str | None,
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    base_url = values.get("ANTHROPIC_BASE_URL") or values.get("BASE_URL")
    api_key = values.get("ANTHROPIC_API_KEY") or values.get("API_KEY")
    auth_token = values.get("ANTHROPIC_AUTH_TOKEN") or values.get("AUTH_TOKEN")
    if not base_url or not (api_key or auth_token) or not model:
        return {**base_result, "ok": False, "status": "smoke_failed", "reason": "missing_base_url_api_key_or_model"}
    endpoint = _anthropic_messages_url(base_url)
    payload = {
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    }
    headers = {
        "content-type": "application/json",
        "anthropic-version": values.get("ANTHROPIC_VERSION") or "2023-06-01",
    }
    headers["authorization"] = f"Bearer {auth_token or api_key}"
    return _http_json_smoke(endpoint, payload, headers, values, base_result, timeout)

def _openai_compatible_smoke(
    values: dict[str, str],
    model: str | None,
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    base_url = values.get("OPENAI_BASE_URL") or values.get("BASE_URL")
    api_key = values.get("OPENAI_API_KEY") or values.get("API_KEY")
    if not base_url or not api_key or not model:
        return {**base_result, "ok": False, "status": "smoke_failed", "reason": "missing_base_url_api_key_or_model"}
    endpoint = _openai_chat_url(base_url)
    payload = {
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    }
    headers = {"content-type": "application/json", "authorization": f"Bearer {api_key}"}
    return _http_json_smoke(endpoint, payload, headers, values, base_result, timeout)

def _http_json_smoke(
    endpoint: str,
    payload: dict[str, Any],
    headers: dict[str, str],
    values: dict[str, str],
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    proxy_info = _proxy_info_for_endpoint(endpoint, values)
    request = urllib.request.Request(
        endpoint,
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    try:
        with _temporary_profile_network_env(values):
            response_ctx = urllib.request.urlopen(request, timeout=timeout)
        with response_ctx as response:
            status = int(getattr(response, "status", 200))
            body = response.read(1024).decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        body = exc.read(4096).decode("utf-8", errors="replace")
        return {
            **base_result,
            **proxy_info,
            "ok": False,
            "status": "smoke_failed",
            "reason": "http_error",
            "http_status": exc.code,
            "endpoint": _redacted_endpoint(endpoint),
            "error": redact_text(body or str(exc)).get("text"),
        }
    except Exception as exc:
        reason = "proxy_connectivity_failed" if proxy_info.get("proxy_configured") else "request_failed"
        return {
            **base_result,
            **proxy_info,
            "ok": False,
            "status": "smoke_failed",
            "reason": reason,
            "endpoint": _redacted_endpoint(endpoint),
            "error": redact_text(str(exc)).get("text"),
            "suggestion": (
                "Proxy is configured for this request; allow the profile BASE_URL through the proxy or disable the proxy for Team Agent startup."
                if proxy_info.get("proxy_configured")
                else "Check BASE_URL network connectivity from this machine."
            ),
        }
    if 200 <= status < 300:
        return {
            **base_result,
            **proxy_info,
            "ok": True,
            "status": "smoke_passed",
            "http_status": status,
            "endpoint": _redacted_endpoint(endpoint),
        }
    return {
        **base_result,
        **proxy_info,
        "ok": False,
        "status": "smoke_failed",
        "reason": "unexpected_status",
        "http_status": status,
        "endpoint": _redacted_endpoint(endpoint),
        "error": redact_text(body).get("text"),
    }

def _anthropic_messages_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/messages"):
        return base
    if base.endswith("/v1"):
        return f"{base}/messages"
    return f"{base}/v1/messages"

def _openai_chat_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/chat/completions"):
        return base
    if base.endswith("/v1"):
        return f"{base}/chat/completions"
    return f"{base}/v1/chat/completions"

def _redacted_endpoint(endpoint: str) -> str:
    return endpoint.split("?", 1)[0]

def _proxy_info_for_endpoint(endpoint: str, values: dict[str, str]) -> dict[str, Any]:
    parsed = urllib.parse.urlparse(endpoint)
    if _profile_proxy_mode(values) == "direct":
        return {"proxy_configured": False, "proxy_mode": "direct"}
    profile_env = _compatible_api_network_exports("compatible_api", values)
    proxy_url = _proxy_url_from_env(parsed.scheme, profile_env)
    if proxy_url:
        return {
            "proxy_configured": True,
            "proxy_scheme": parsed.scheme,
            "proxy_url": _redact_proxy_url(proxy_url),
            "proxy_source": "profile",
        }
    ambient_proxy_url = _proxy_url_from_env(parsed.scheme, os.environ)
    if ambient_proxy_url:
        return {
            "proxy_configured": True,
            "proxy_scheme": parsed.scheme,
            "proxy_url": _redact_proxy_url(ambient_proxy_url),
            "proxy_source": "ambient",
        }
    return {"proxy_configured": False}

def _proxy_url_from_env(scheme: str, env: Any) -> str | None:
    upper = f"{scheme.upper()}_PROXY"
    lower = f"{scheme.lower()}_proxy"
    return env.get(upper) or env.get(lower) or env.get("ALL_PROXY") or env.get("all_proxy")

@contextmanager
def _temporary_profile_network_env(values: dict[str, str]) -> Any:
    profile_env = _compatible_api_network_exports("compatible_api", values)
    direct = _profile_proxy_mode(values) == "direct"
    touched_keys = COMPATIBLE_API_NETWORK_ENV_KEYS if direct else tuple(profile_env)
    saved = {key: os.environ.get(key) for key in touched_keys}
    try:
        if direct:
            for key in COMPATIBLE_API_NETWORK_ENV_KEYS:
                os.environ.pop(key, None)
        os.environ.update(profile_env)
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

def _redact_proxy_url(proxy_url: str) -> str:
    parsed = urllib.parse.urlparse(proxy_url)
    if not parsed.netloc:
        return proxy_url
    host = parsed.hostname or ""
    port = f":{parsed.port}" if parsed.port else ""
    auth = "[redacted]@" if parsed.username or parsed.password else ""
    return urllib.parse.urlunparse((parsed.scheme, f"{auth}{host}{port}", parsed.path, "", "", ""))
