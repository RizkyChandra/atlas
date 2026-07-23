#!/usr/bin/env bash
# Backlog-ledger coverage gate (M10, informational).
#
# BACKLOG.md rows look like:
#   | PR #2142 | bug-fix | M2 | extract::indirect_call_class_refs | fold | ... |
# columns: id | bucket | milestone | test-id | status | description
#
# For every row with status `fold` we check whether its test-id's function name
# (the segment after the last `::`) appears anywhere under crates/. We print a
# coverage %. This is INFORMATIONAL — it never fails the build (see `exit 0`).
#
# ponytail: literal token match, no fuzzy scoring — a folded requirement counts
# as covered iff its test fn name is grep-able in the tree.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BACKLOG="$ROOT/BACKLOG.md"
SRC="$ROOT/crates"

total=0
covered=0
missing=()

while IFS='|' read -r _ _id _bucket _ms testid status _rest; do
  status="$(echo "$status" | tr -d '[:space:]')"
  [ "$status" = "fold" ] || continue
  testid="$(echo "$testid" | tr -d '[:space:]')"
  [ -n "$testid" ] || continue
  fn="${testid##*::}"
  total=$((total + 1))
  if grep -rqw "$fn" "$SRC" 2>/dev/null; then
    covered=$((covered + 1))
  else
    missing+=("$testid")
  fi
done < <(grep -E '^\| *(PR|issue) #' "$BACKLOG")

if [ "$total" -eq 0 ]; then
  echo "ledger-gate: no fold rows found in BACKLOG.md"
  exit 0
fi

pct=$(awk "BEGIN{printf \"%.1f\", 100*$covered/$total}")
echo "ledger-gate: $covered/$total fold rows have a matching test symbol in crates/ (${pct}%)"
echo "ledger-gate: informational only — not failing the build."
exit 0
