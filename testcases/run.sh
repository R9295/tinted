#!/usr/bin/env bash
# Runs every package in testcases/ through the same pipeline as a real
# project: build it and its dependencies to LLVM IR, link the IR, and run
# tinted-ir on the linked module.
#
# Each case's src/main.rs records:
#   // want:  OK|FAIL   what a perfect tool would say
#   // today: OK|FAIL   what tinted says now
# A case passes when the analyzer agrees with `want`. The three explicitly
# marked `invariant` cases instead check `today`: validation predicates,
# ordering and value identity remain trusted contracts, not proven facts.
#
# Needs nightly cargo and the LLVM 23 development files. LLVM_LINK overrides
# llvm-link-23, and LLVM_SYS_231_PREFIX the LLVM that tinted-ir is built
# against. Pass case names to run only those.
set -euo pipefail

cd "$(dirname "$0")/.."
LLVM_LINK=${LLVM_LINK:-llvm-link-23}
TARGET=x86_64-unknown-linux-gnu
out=$PWD/target/testcases

cargo build -q
(cd tinted-ir && cargo build -q --release)

if [[ $# -gt 0 ]]; then
  cases=("$@")
else
  cases=()
  for dir in testcases/*/; do
    [[ -f "$dir/Cargo.toml" ]] || continue
    cases+=("$(basename "$dir")")
  done
fi

failed=0
for name in "${cases[@]}"; do
  main=testcases/$name/src/main.rs
  want=$(sed -n 's|^// want: *||p' "$main")
  today=$(sed -n 's|^// today: *||p' "$main")
  invariant=$(sed -n 's|^// invariant: *||p' "$main")
  expected=$want
  [[ -z $invariant ]] || expected=$today

  # A target dir per case, so that its IR is only its own crates' IR. --target
  # keeps the flags off the tinted proc macro, which is built for the host.
  # The directory is wiped so that no stale IR from an earlier build with
  # different flags ends up in the link.
  dir=$out/$name
  rm -rf "$dir"
  CARGO_INCREMENTAL=0 \
  RUSTFLAGS="--emit=llvm-ir -C codegen-units=1 -C opt-level=1 -Cpanic=abort" \
  cargo build -q --manifest-path testcases/Cargo.toml -p "$name" \
    --target "$TARGET" --target-dir "$dir"
  mkdir -p "$dir/ir"
  find "$dir/$TARGET" -name "*.ll" -exec cp {} "$dir/ir/" \;

  "$LLVM_LINK" "$dir"/ir/*.ll -o "$dir/all.bc"
  status=0
  timeout 60 tinted-ir/target/release/tinted-ir "$dir/all.bc" > "$dir/check.log" 2>&1 || status=$?
  if [[ $status == 0 ]]; then
    got=OK
  elif [[ $status == 1 ]]; then
    got=FAIL
  else
    echo "ERROR    $name: analyzer exited with status $status (see $dir/check.log)"
    failed=1
    continue
  fi

  if [[ $got != "$expected" ]]; then
    echo "FAIL     $name: tinted says $got, expected $expected (see $dir/check.log)"
    failed=1
  elif [[ -n $invariant ]]; then
    echo "invariant $name: $got (want $want; $invariant)"
  else
    echo "PASS     $name: $got"
  fi
done
exit $failed
