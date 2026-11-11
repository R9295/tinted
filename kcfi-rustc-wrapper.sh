#!/usr/bin/env bash
# RUSTC_WRAPPER for cfg.sh. Runs rustc as asked, except that -Zsanitizer=kcfi
# is dropped for the crates named in $KCFI_SKIP_CRATES (space separated, as
# passed to --crate-name): crates that rustc cannot compile with KCFI.
rustc=$1
shift

name=
prev=
for arg in "$@"; do
  if [[ $prev == --crate-name ]]; then
    name=$arg
  fi
  prev=$arg
done

if [[ -n $name && " ${KCFI_SKIP_CRATES:-} " == *" $name "* ]]; then
  args=()
  for arg in "$@"; do
    if [[ $arg != -Zsanitizer=kcfi ]]; then
      args+=("$arg")
    fi
  done
  exec "$rustc" "${args[@]}"
fi
exec "$rustc" "$@"
