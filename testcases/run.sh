#!/usr/bin/env bash
# Runs every package in testcases/ through the same pipeline as a real
# project: build it and its dependencies to LLVM IR with KCFI, link the IR,
# dump the call graph, resolve indirect calls and run tinted-check.
#
# Each case is a small program that tinted gets wrong. Its src/main.rs starts
# with two header lines:
#   // want:  OK|FAIL   what a perfect tool would say
#   // today: OK|FAIL   what tinted says now
# A case passes when tinted still says what `today` records, so a failure
# means its behaviour changed: either a gap was closed (then update `today`,
# or move the case out once it equals `want`) or something regressed.
#
# Needs a nightly cargo and LLVM's llvm-link and opt. Override the LLVM tools
# with LLVM_LINK and OPT. Pass case names to run only those.
set -euo pipefail

cd "$(dirname "$0")/.."
LLVM_LINK=${LLVM_LINK:-llvm-link-23}
OPT=${OPT:-opt-23}
TARGET=x86_64-unknown-linux-gnu
out=$PWD/target/testcases

cargo build -q

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

  # A target dir per case, so that its IR is only its own crates' IR. --target
  # keeps the flags off the tinted proc macro, which is built for the host.
  dir=$out/$name
  rm -rf "$dir/ir"
  CARGO_INCREMENTAL=0 \
  RUSTFLAGS="--emit=llvm-ir -C codegen-units=1 -C opt-level=0 -Zsanitizer=kcfi -Cpanic=abort -Cunsafe-allow-abi-mismatch=sanitizer" \
  cargo build -q --manifest-path testcases/Cargo.toml -p "$name" \
    --target "$TARGET" --target-dir "$dir"
  mkdir -p "$dir/ir"
  find "$dir/$TARGET" -name "*.ll" -exec cp {} "$dir/ir/" \;

  "$LLVM_LINK" "$dir"/ir/*.ll -o "$dir/all.bc"
  "$OPT" -passes=print-callgraph -disable-output "$dir/all.bc" 2> "$dir/all.cfp"
  target/debug/tinted-resolve "$dir"/ir/*.ll > "$dir/all.indirect.cfp" 2> /dev/null

  if target/debug/tinted-check "$dir/all.cfp" "$dir/all.indirect.cfp" > "$dir/check.log"; then
    got=OK
  else
    got=FAIL
  fi

  if [[ $got != "$today" ]]; then
    echo "CHANGED  $name: tinted says $got, recorded today: $today (want: $want)"
    failed=1
  elif [[ $got == "$want" ]]; then
    echo "covered  $name: $got"
  else
    echo "gap      $name: tinted says $got, want $want"
  fi
done
exit $failed
