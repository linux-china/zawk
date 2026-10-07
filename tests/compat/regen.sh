#!/usr/bin/env bash
# Generate the expected output (`cases/*.out`) of the compatibility test cases with gawk.
#
#   ./regen.sh            regenerate all expected outputs
#   ./regen.sh NAME...    regenerate the given cases (names without the .awk suffix)
#   ./regen.sh --check    verify that the committed expected outputs match gawk (used by CI)
#
# Directives in the leading comments of a case:
#   # stdin: FILE     file used as standard input (default: empty input)
#   # args: ARGS      operands after the program file (files, var=value assignments)
#   # opts: OPTS      options before `-f`, e.g. `-F:` or `-v x=1`
#   # exit: N         expected exit code (default: 0)
set -euo pipefail

cd "$(dirname "$0")"
export LC_ALL=C.UTF-8
GAWK=${GAWK:-gawk}

directive() {
  sed -n "s/^# $1: //p" "$2" | head -n 1
}

run_case() {
  local awk_file=$1 out_file=$2
  local stdin opts args expected_code code
  stdin=$(directive stdin "$awk_file")
  opts=$(directive opts "$awk_file")
  args=$(directive args "$awk_file")
  expected_code=$(directive exit "$awk_file")
  set +e
  # shellcheck disable=SC2086 # opts and args are word-split on purpose
  "$GAWK" $opts -f "$awk_file" $args < "${stdin:-/dev/null}" > "$out_file" 2> /dev/null
  code=$?
  set -e
  if [[ $code -ne ${expected_code:-0} ]]; then
    echo "$awk_file: gawk exited with $code, expected ${expected_code:-0}" >&2
    return 1
  fi
}

check=0
names=()
for arg in "$@"; do
  if [[ $arg == --check ]]; then check=1; else names+=("$arg"); fi
done
if [[ ${#names[@]} -eq 0 ]]; then
  for f in cases/*.awk; do names+=("$(basename "$f" .awk)"); done
fi

failed=0
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
for name in "${names[@]}"; do
  awk_file=cases/$name.awk
  if [[ $check -eq 1 ]]; then
    if ! run_case "$awk_file" "$tmp" || ! diff -u "cases/$name.out" "$tmp"; then
      echo "MISMATCH: $name" >&2
      failed=1
    fi
  else
    run_case "$awk_file" "cases/$name.out" || failed=1
  fi
done
exit $failed
