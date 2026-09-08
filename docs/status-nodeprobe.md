# Status nodeprobe capability receipt

`team-agent status` may execute `nodeprobe` only after an operator-trusted
capability receipt has been paired with the exact binary. The receipt prevents
accidental binary/receipt mismatch; it is **not** a defense against a local
writer who can replace both files. No signature, key, notarization, or runtime
network lookup is part of this contract.

## Accepted producer contract

PR138 currently accepts the public producer contract from:

- repository: `Florious95/team-agent-scratch/nodeprobe`
- source commit: `ff316dc0afe8ab280e61d30934e7624579be6224`
- source tree: `5217a41aa914ddcb72c27f39f1b4af9ead68b1b6`
- report envelope: schema `1`
- allowed operations: `tmux.list-panes`, `ps.pid_ppid_stat_comm`
- forbidden operations: `tmux.capture-pane`, `tmux.attach`, `tmux.send-keys`,
  `process.argv`, `pane_body`

The source contract is independently documented in the accepted producer's
`src/lib.rs`, `README.md`, and `docs/api-architecture.md`. A future producer
contract must update the Team Agent release constants and its receipt contract;
status does not infer new capabilities from a binary.

## Receipt pairing

The installer places these files in one directory:

```text
nodeprobe
nodeprobe.capability.json
```

The JSON fields are versioned by `schema: "nodeprobe-capability-v1"` and include
`binary`, `binary_sha256`, `source_repo`, `source_commit`, `source_tree`,
`target`, `report_schema`, `capabilities`, and `forbidden`. The binary hash is
computed from the installed bytes. The resolver checks the sibling receipt,
actual SHA-256, accepted source identity, current target, report schema, and
exact narrow capability sets before it invokes the binary. Missing or invalid
receipt means `unknown` and no child process.

The existing CI build receipt remains the provenance input. For example, the
accepted arm64 artifact receipt is:

```text
/Volumes/nvme/tmp/nodeprobe-rollout-ff316dc/artifact/nodeprobe-build-receipt.txt
```

It records the source commit/tree, target, and `binary_sha256`; the downloaded
artifact was recorded separately at:

```text
/Volumes/nvme/tmp/nodeprobe-rollout-ff316dc/artifact/nodeprobe
```

These are evidence paths from one verified rollout, not Team Agent defaults and
not a claim about the currently installed machine. Do not substitute an
arbitrary binary, source commit, or hand-written hash.

## Reproducible operator conversion

An authorized installer converts a downloaded, verified CI pair—not an
unknown local binary—using the following bounded Python procedure. It refuses
if the real build receipt is absent, the source identity differs, the receipt's
binary path is absent, or the downloaded bytes do not equal the receipt hash.
The output must be installed atomically beside that same binary.

```sh
BUILD_RECEIPT=/path/from/verified-ci/nodeprobe-build-receipt.txt \
BINARY=/path/from/verified-ci/nodeprobe \
OUT=/path/from/verified-ci/nodeprobe.capability.json \
python3 - <<'PY'
import hashlib, json, os

expected = {
    "source_commit": "ff316dc0afe8ab280e61d30934e7624579be6224",
    "source_tree": "5217a41aa914ddcb72c27f39f1b4af9ead68b1b6",
    "target": "aarch64-apple-darwin",
}
receipt_path = os.environ["BUILD_RECEIPT"]
binary_path = os.environ["BINARY"]
out_path = os.environ["OUT"]
fields = {}
with open(receipt_path, encoding="utf-8") as stream:
    for line in stream:
        key, sep, value = line.rstrip("\n").partition("=")
        if sep:
            fields[key] = value
for key, value in expected.items():
    if fields.get(key) != value:
        raise SystemExit(f"unexpected verified producer field: {key}")
if not os.path.isfile(binary_path) or os.path.basename(binary_path) != "nodeprobe":
    raise SystemExit("verified CI binary is missing or is not nodeprobe")
actual = hashlib.sha256(open(binary_path, "rb").read()).hexdigest()
if actual != fields.get("binary_sha256"):
    raise SystemExit("CI receipt hash does not match downloaded binary")
manifest = {
    "schema": "nodeprobe-capability-v1",
    "binary": "nodeprobe",
    "binary_sha256": actual,
    "source_repo": "Florious95/team-agent-scratch/nodeprobe",
    "source_commit": fields["source_commit"],
    "source_tree": fields["source_tree"],
    "target": fields["target"],
    "report_schema": 1,
    "capabilities": ["tmux.list-panes", "ps.pid_ppid_stat_comm"],
    "forbidden": [
        "tmux.capture-pane", "tmux.attach", "tmux.send-keys",
        "process.argv", "pane_body",
    ],
}
tmp = out_path + ".tmp"
with open(tmp, "w", encoding="utf-8") as stream:
    json.dump(manifest, stream, sort_keys=True, separators=(",", ":"))
    stream.write("\n")
os.replace(tmp, out_path)
PY
```

The pinned source fields in this example are the accepted contract for this
Team Agent release; a different producer must go through a separately reviewed
contract update. The operator-trusted boundary is intentional and documented:
matching hash prevents accidental pairing, but cannot protect against a user
who can write both the executable and its receipt.

## Corpus and fail-closed behavior

The producer also needs its canonical title/provider corpus. CI checkout paths
are not runtime defaults. An installation must provide the already accepted
canonical corpus mechanism (or explicit readable corpus environment) without
inventing a path in status. If corpus data is unavailable, the producer's
structured error and the status projection remain unknown; status never
executes an unbound binary merely to discover that fact.
