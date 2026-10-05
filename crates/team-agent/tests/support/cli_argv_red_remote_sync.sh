#!/usr/bin/env bash
# Test-only ordinary Git incremental synchronization after a full offload sync.
# Does not change the installed offload helper or weaken its manifest/run gates.
set -euo pipefail
src=${1:?source clean normal clone}
unit=${2:?single-use target unit}
base_unit=${3:?verified completed baseline unit}
case "$unit:$base_unit" in gb-86a4d431-argv-red-a2:gb-86a4d431-argv-red-a1|gb-86a4d431-argv-red-a3:gb-86a4d431-argv-red-a2|gb-86a4d431-argv-red-a4:gb-86a4d431-argv-red-a3) ;; *) echo 'unowned unit refused' >&2; exit 2;; esac
git -C "$src" diff --quiet
git -C "$src" diff --cached --quiet
sha=$(git -C "$src" rev-parse HEAD)
root=/workspace/grok-bot-offload/jobs
stage=.sync-stage-$$-$RANDOM
ssh grok-bot bash -s -- "$unit" "$base_unit" <<'REMOTE'
set -euo pipefail
root=/workspace/grok-bot-offload/jobs
unit=$1; base=$2
[ -f /workspace/grok-bot-offload/.gb-offload-root ]
[ "$(realpath -e "$root/$unit")" = "$root/$unit" ]
[ ! -e "$root/$unit/work" ]
[ -d "$root/$base/work/.git" ]
[ ! -e "$root/$unit/.source.git" ]
git clone --bare --no-hardlinks "$root/$base/work" "$root/$unit/.source.git"
REMOTE
git -C "$src" push "ssh://grok-bot$root/$unit/.source.git" HEAD:refs/heads/argv-red
ssh grok-bot bash -s -- "$unit" "$stage" "$sha" <<'REMOTE'
set -euo pipefail
root=/workspace/grok-bot-offload/jobs
unit=$1; stage=$2; sha=$3
work=$root/$unit/$stage
[ ! -e "$work" ]
git clone --no-hardlinks --branch argv-red "$root/$unit/.source.git" "$work"
[ "$(git -C "$work" rev-parse HEAD)" = "$sha" ]
git -C "$work" diff --quiet
git -C "$work" diff --cached --quiet
REMOTE
"$HOME/.agents/skills/grok-bot-tests/scripts/verify-sync-manifest.sh" --src "$src" --unit "$unit" --remote-work "$root/$unit/$stage"
ssh grok-bot bash -s -- "$unit" "$stage" "$sha" <<'REMOTE'
set -euo pipefail
root=/workspace/grok-bot-offload/jobs
unit=$1; stage=$2; sha=$3
work=$root/$unit/$stage
cd "$work"
git bundle create .source-history.bundle HEAD
git bundle verify .source-history.bundle
bundle_sha=$(sha256sum .source-history.bundle | awk '{print $1}')
[ "$(git rev-parse HEAD)" = "$sha" ]
printf 'SourceCommit=%s\nSourceBundleSHA256=%s\n' "$sha" "$bundle_sha" >> .sync-verified
printf '/.gb-tracked-manifest\n/.sync-verified\n' > .git/info/exclude
rm .source-history.bundle
[ ! -e "$root/$unit/work" ]
mv "$work" "$root/$unit/work"
printf 'GitSyncVerified=yes\nSourceCommit=%s\nSourceBundleSHA256=%s\n' "$sha" "$bundle_sha"
REMOTE
