#!/usr/bin/env bash
# -------------------------------------------------------------------
# validate_examples.sh
#
# Validates all .dol example files by parsing them with `dol --explain`.
# The --explain flag shows the query plan without executing, so this
# works without a running Docker daemon.
#
# Usage:
#   ./scripts/validate_examples.sh              # uses cargo run
#   DOL=./target/release/dol ./scripts/validate_examples.sh  # specific binary
# -------------------------------------------------------------------

set -euo pipefail

EXAMPLES_DIR="examples"

# Check we're in the project root
if [ ! -d "$EXAMPLES_DIR" ]; then
    echo "Error: '$EXAMPLES_DIR' directory not found."
    echo "Run this script from the project root (e.g., ./scripts/validate_examples.sh)"
    exit 1
fi

# Check there are .dol files to validate
shopt -s nullglob
DOL_FILES=("$EXAMPLES_DIR"/*.dol)
if [ ${#DOL_FILES[@]} -eq 0 ]; then
    echo "Error: no .dol files found in '$EXAMPLES_DIR/'"
    exit 1
fi
shopt -u nullglob

# Determine which binary to use
if [ -n "${DOL:-}" ]; then
    DOL_BIN="$DOL"
elif command -v dol &>/dev/null; then
    DOL_BIN="dol"
else
    echo "Note: no 'dol' binary found, using 'cargo run --quiet --'"
    echo "      (this may compile the project on first run)"
    echo ""
    DOL_BIN="cargo run --quiet --"
fi

PASSED=0
FAILED=0
FAILED_FILES=()

echo "============================================"
echo "  Validating DOL example files"
echo "  Binary: $DOL_BIN"
echo "============================================"
echo ""

for file in "${DOL_FILES[@]}"; do
    name=$(basename "$file")
    query=$(cat "$file")

    # Run dol --explain on the query; capture both stdout and stderr
    if output=$($DOL_BIN --explain "$query" 2>&1); then
        echo "  [PASS] $name"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name"
        echo "         Error: $output"
        FAILED=$((FAILED + 1))
        FAILED_FILES+=("$name")
    fi
done

echo ""
echo "============================================"
printf "  Results: %d passed, %d failed\n" "$PASSED" "$FAILED"
echo "============================================"

if [ "$FAILED" -gt 0 ]; then
    echo ""
    echo "  Failed files:"
    for f in "${FAILED_FILES[@]}"; do
        echo "    - $f"
    done
    exit 1
fi
