#!/usr/bin/env bash
# Build the agent-training dataset from the corpus, and validate it.
#
#   ./scripts/dataset.sh            regenerate into ./dataset
#   ./scripts/dataset.sh --check    rebuild in a scratch directory and fail if
#                                   the committed dataset no longer replays
#
# The pipeline is four commands, and each one is a gate on the next:
#
#   put       the starting workbook, addressed by the hash of its contents
#   record    a human demonstration per task, dropping any that fails its grader
#   augment   every demonstration by every recipe, dropping variants that fail
#   validate  replay the lot and check every recorded state hash
#
# `validate` is the one that belongs in CI. A trajectory that stops replaying
# is either corrupt or the engine moved underneath it, and either way it has to
# stop being training data before it teaches something that is no longer true.
set -euo pipefail

cd "$(dirname "$0")/.."

CORPUS=corpus/env
OUT=${OUT:-dataset}
CHECK=0
[[ "${1:-}" == "--check" ]] && { CHECK=1; OUT=$(mktemp -d); }

ENV_BIN=(cargo run -q -p env --bin gridline-env --)

if [[ $CHECK -eq 1 ]]; then
  echo "==> validating the committed dataset"
  "${ENV_BIN[@]}" validate --store dataset/snapshots \
    --dataset dataset/demonstrations.jsonl --tasks dataset/tasks.jsonl >/dev/null
  "${ENV_BIN[@]}" validate --store dataset/snapshots \
    --dataset dataset/variants.jsonl >/dev/null
  echo "the committed dataset still replays"
  exit 0
fi

rm -rf "$OUT"
mkdir -p "$OUT/snapshots"

echo "==> storing the starting workbook"
LEDGER=$("${ENV_BIN[@]}" put --store "$OUT/snapshots" "$CORPUS/ledger.start.jsonl" | cut -f1)
echo "    ledger = $LEDGER"

sed "s/{{ledger}}/$LEDGER/g" "$CORPUS/tasks.template.jsonl" > "$OUT/tasks.jsonl"

echo "==> recording the demonstrations"
# One task per recording: `record` replays one action log against the tasks it
# is given, and these two demonstrations are different sequences.
head -1 "$OUT/tasks.jsonl" > "$OUT/.task-totals.jsonl"
tail -1 "$OUT/tasks.jsonl" > "$OUT/.task-grand.jsonl"
"${ENV_BIN[@]}" record --store "$OUT/snapshots" \
  --tasks "$OUT/.task-totals.jsonl" --actions "$CORPUS/ledger.totals.jsonl" \
  --out "$OUT/demonstrations.jsonl"
"${ENV_BIN[@]}" record --store "$OUT/snapshots" \
  --tasks "$OUT/.task-grand.jsonl" --actions "$CORPUS/ledger.grand-total.jsonl" \
  --out "$OUT/demonstrations.jsonl"
rm -f "$OUT/.task-totals.jsonl" "$OUT/.task-grand.jsonl"

echo "==> augmenting"
# A non-zero rejection count here is the system working: a recipe that cannot
# be remapped correctly for a given demonstration is refused rather than
# shipped, and the reasons are printed above.
"${ENV_BIN[@]}" augment --store "$OUT/snapshots" \
  --tasks "$OUT/tasks.jsonl" --dataset "$OUT/demonstrations.jsonl" \
  --recipes "$CORPUS/recipes.json" --out "$OUT/variants.jsonl" \
  --out-tasks "$OUT/variant-tasks.jsonl"

echo "==> validating"
"${ENV_BIN[@]}" validate --store "$OUT/snapshots" \
  --dataset "$OUT/demonstrations.jsonl" --tasks "$OUT/tasks.jsonl" >/dev/null
"${ENV_BIN[@]}" validate --store "$OUT/snapshots" \
  --dataset "$OUT/variants.jsonl" >/dev/null

echo
echo "$OUT:"
echo "  $(wc -l < "$OUT/demonstrations.jsonl") demonstration(s)"
echo "  $(wc -l < "$OUT/variants.jsonl") validated variant(s)"
echo "  $(wc -l < "$OUT/variant-tasks.jsonl") variant task(s) to attempt"
echo "  $(ls "$OUT/snapshots" | wc -l) snapshot(s), $(du -sh "$OUT/snapshots" | cut -f1)"
