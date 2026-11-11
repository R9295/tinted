#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
LLVM_CONFIG=${LLVM_CONFIG:-llvm-config-23}
CXX=${CXX:-clang++-23}
mkdir -p target/passes
read -r -a cxxflags <<< "$("$LLVM_CONFIG" --cxxflags)"
read -r -a ldflags <<< "$("$LLVM_CONFIG" --ldflags)"
read -r -a libs <<< "$("$LLVM_CONFIG" --libs core irreader analysis support)"
"$CXX" "${cxxflags[@]}" -std=c++20 -O2 -g passes/tinted-ir.cpp \
  "${ldflags[@]}" "${libs[@]}" -o target/passes/tinted-ir
