from __future__ import annotations


STATUS_TEXT_LIMIT = 240
STATUS_EVENT_LIMIT = 3
PEEK_MAX_LINES = 80
PEEK_SEARCH_SCAN_LINES = 300
PEEK_MAX_MATCHES = 5
APPROVAL_SCAN_LINES = 120

PENDING_DELIVERY_STATUSES = {
    "pending",
    "accepted",
    "queued_until_idle",
    "queued_until_start",
    "queued_stopped",
    "queued_pane_missing",
}
