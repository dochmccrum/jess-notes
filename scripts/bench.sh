#!/usr/bin/env bash
# DESIGN §18 benchmarks: generates the vault (tools/vaultgen), imports it, runs the server, browser
# and bundle benchmarks, and compares the results with bench/thresholds.json.
#
#   scripts/bench.sh            # everything (~10 min here; the vault is cached between runs)
#   SEED=2 scripts/bench.sh     # another vault
#
# Needs the built UI (ui/dist) and Playwright's Chromium. Work files are in target/bench/.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
out=$root/target/bench
mkdir -p "$out"
seed=${SEED:-1}
results=$out/results.jsonl
: >"$results"
export BENCH_OUT=$results

cargo build -q --release -p vaultgen -p jess-server
jess=$root/target/release/jess
now() { date +%s.%N; }
secs() { echo "$1 $2" | awk '{printf "%.1f", $2 - $1}'; }
rec() { printf '{"key":"%s","value":%s,"unit":"%s"}\n' "$1" "$2" "$3" >>"$results"; echo "BENCH $1 = $2 $3"; }

# The vaults: same seed, same bytes, so they are generated once.
vault=$out/vault-$seed
if [ ! -f "$vault/.done" ]; then
  rm -rf "$vault"
  target/release/vaultgen --out "$vault" --seed "$seed"
  touch "$vault/.done"
fi
small=$out/vault5k-$seed
if [ ! -f "$small/.done" ]; then
  rm -rf "$small"
  target/release/vaultgen --out "$small" --seed "$seed" --notes 5000 --attachments 0 --pdfs 0 --large-pdf none --big-folder 0
  touch "$small/.done"
fi

# Server: five clients typing. First, on a quiet disk: its commits are fsync-bound, and the
# imports below leave a gigabyte of writeback behind them.
sync
cargo test -q --release -p jess-server --test bench -- --ignored --nocapture 2>&1 | grep '^BENCH' || true

# Import: 5k notes, then the full vault (into the data dir the browser benchmarks use).
port=$((18950 + RANDOM % 40))
for v in small full; do
  data=$out/data-$v
  rm -rf "$data"
  src=$([ $v = small ] && echo "$small" || echo "$vault")
  t=$(now)
  JESS_DATA_DIR=$data PORT=$port JESS_GIT_ENABLED=false JESS_MIRROR_ENABLED=false "$jess" import "$src" >/dev/null
  if [ $v = small ]; then rec import_5k_notes_s "$(secs "$t" "$(now)")" s; else rec import_full_vault_s "$(secs "$t" "$(now)")" s; fi
done
# Thumbnails and PDF text, as a server would have them (it derives in the background otherwise).
t=$(now)
JESS_DATA_DIR=$out/data-full PORT=$port "$jess" derive-all >/dev/null
rec derive_all_s "$(secs "$t" "$(now)")" s

# Bundle sizes.
(cd ui && node scripts/check-budgets.mjs >/dev/null) || true

# Browser (after the imports' writeback).
sync
(cd ui && BENCH_DATA=$out/data-full BENCH_PORT=$port BENCH_PROFILE=$out/profile JESS_BIN=$jess npx playwright test -c bench/bench.config.ts) || true

node scripts/bench-check.mjs "$results"
