#!/bin/sh
set -eu

[ "$#" -eq 0 ] || { echo 'BUILD_DECLARATION_INVALID: usage: check-build-environment.sh' >&2; exit 78; }
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
rust_expected=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' "$root/rust-toolchain.toml")
rust_actual=$(rustc --version 2>/dev/null | awk '{print $2}' || true)
rust_host=$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)
python_expected=$(awk 'NF { value=$0; count++ } END { if (count == 1) print value; else exit 1 }' "$root/.python-version" 2>/dev/null || true)
python_actual=$(python3 -c 'import platform; print(platform.python_version())' 2>/dev/null || true)
python_machine=$(python3 -c 'import platform; print(platform.machine())' 2>/dev/null || true)

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) required_host=aarch64-apple-darwin; required_python=arm64 ;;
  Darwin-x86_64) if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then required_host=aarch64-apple-darwin; required_python=arm64; else required_host=x86_64-apple-darwin; required_python=x86_64; fi ;;
  Linux-aarch64|Linux-arm64) required_host=aarch64-unknown-linux-gnu; required_python=aarch64 ;;
  Linux-x86_64) required_host=x86_64-unknown-linux-gnu; required_python=x86_64 ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) required_host=x86_64-pc-windows-msvc; required_python=AMD64 ;;
  *) echo 'TOOLCHAIN_MISMATCH: unsupported host' >&2; exit 78 ;;
esac

if [ -z "$rust_expected" ] || [ "$rust_actual" != "$rust_expected" ] || [ "$rust_host" != "$required_host" ] || \
   [ -z "$python_expected" ] || [ "$python_actual" != "$python_expected" ] || [ "$python_machine" != "$required_python" ]; then
  printf 'TOOLCHAIN_MISMATCH: expected rust=%s/%s python=%s/%s; actual rust=%s/%s python=%s/%s\n' \
    "${rust_expected:-missing}" "$required_host" "${python_expected:-missing}" "$required_python" \
    "${rust_actual:-missing}" "${rust_host:-unknown}" "${python_actual:-missing}" "${python_machine:-unknown}" >&2
  exit 78
fi
printf 'BUILD_ENVIRONMENT_READY rust=%s/%s python=%s/%s\n' "$rust_actual" "$rust_host" "$python_actual" "$python_machine"
