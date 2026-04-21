#!/bin/bash
# Install sigilbuzz's committed git hooks into .git/hooks/.
#
# Run once after cloning. Idempotent.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

HOOKS_SRC="hooks"
HOOKS_DST=".git/hooks"

if [ ! -d "$HOOKS_SRC" ]; then
    echo "error: $HOOKS_SRC not found" >&2
    exit 1
fi

for hook in "$HOOKS_SRC"/*; do
    name="$(basename "$hook")"
    target="$HOOKS_DST/$name"

    rm -f "$target"
    ln -s "../../$hook" "$target"
    chmod +x "$hook"
    echo "installed $name -> $target"
done

echo "done."
