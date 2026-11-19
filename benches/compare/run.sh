#!/usr/bin/env bash
# Runs the bench origin, proxy-net, and whichever of nginx, HAProxy, Envoy,
# and Pingora are present, then the same closed-loop load against each.
# Prints a Markdown table. Used by .github/workflows/bench.yml (Linux).
#
#   bash benches/compare/run.sh [seconds] [connection counts...]
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 2
SECONDS_EACH="${1:-10}"
shift || true
COUNTS=("${@:-1 16 64}")
read -r -a COUNTS <<< "${COUNTS[*]}"

cargo bench --bench compare --no-run --quiet 2>/dev/null
BIN=$(find target/release/deps -maxdepth 1 -type f -name 'compare-*' ! -name '*.d' | head -1)
[ -x "$BIN" ] || { echo "the bench binary was not built"; exit 2; }

PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  docker rm -f envoy-bench >/dev/null 2>&1 || true
}
trap cleanup EXIT

wait_port() { # port: true once something accepts on it, within 20 s
  for _ in $(seq 1 200); do
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && return 0
    sleep 0.1
  done
  return 1
}

"$BIN" origin 127.0.0.1:9000 & PIDS+=($!)
wait_port 9000 || { echo "the origin did not start"; exit 2; }

TARGETS=("direct:9000")
"$BIN" proxy 127.0.0.1:8080 127.0.0.1:9000 & PIDS+=($!)
wait_port 8080 && TARGETS+=("proxy-net:8080")

if command -v nginx >/dev/null; then
  mkdir -p /tmp/nginx-bench
  nginx -p /tmp/nginx-bench -e /tmp/nginx-bench/error.log -c "$PWD/benches/compare/nginx.conf" & PIDS+=($!)
  wait_port 8081 && TARGETS+=("nginx:8081")
fi
if command -v haproxy >/dev/null; then
  haproxy -f benches/compare/haproxy.cfg & PIDS+=($!)
  wait_port 8082 && TARGETS+=("haproxy:8082")
fi
if command -v docker >/dev/null && docker image inspect "${ENVOY_IMAGE:-none}" >/dev/null 2>&1; then
  docker run -d --rm --name envoy-bench --network host \
    -v "$PWD/benches/compare/envoy.yaml:/etc/envoy/envoy.yaml:ro" "$ENVOY_IMAGE" >/dev/null
  wait_port 8083 && TARGETS+=("envoy:8083")
fi
PINGORA=benches/compare/pingora/target/release/pingora-bench
if [ -x "$PINGORA" ]; then
  "$PINGORA" -c benches/compare/pingora/conf.yaml & PIDS+=($!)
  wait_port 8084 && TARGETS+=("pingora:8084")
fi

echo "Linux, $(nproc) CPUs, ${SECONDS_EACH} s per row after a 1 s warm-up; the load generator, the proxy, and the origin share the machine."
echo
echo "| target | requests/s | p50 | p99 | p99.9 | errors |"
echo "|---|---:|---:|---:|---:|---:|"
for c in "${COUNTS[@]}"; do
  for t in "${TARGETS[@]}"; do
    name="${t%%:*}"
    port="${t##*:}"
    "$BIN" load "127.0.0.1:$port" --connections "$c" --seconds "$SECONDS_EACH" --label "$name, $c conn"
  done
done
