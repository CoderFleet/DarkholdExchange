#!/usr/bin/env bash
set -u
fail=0
want_rust=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
want_node=$(tr -d 'v \n' < .nvmrc)

chk() { # label have want-prefix
  if [[ "$2" == "$3"* ]]; then echo "ok    $1  $2"; else echo "FAIL  $1  have '$2', want '$3'"; fail=1; fi
}
need() {
  if command -v "$1" >/dev/null 2>&1; then echo "ok    $1"; else echo "FAIL  $1 not found"; fail=1; fi
}

chk rust "$(rustc -V 2>/dev/null | cut -d' ' -f2)" "$want_rust"
chk node "$(node -v 2>/dev/null | sed 's/^v//')" "$want_node"
for t in npm cargo-nextest just docker git k6; do need "$t"; done
exit $fail
