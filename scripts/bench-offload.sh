#!/usr/bin/env bash
# bench-offload.sh
#
# Benchmarks netstack-smoltcp's forward examples with 2-stream iperf3.
# Compares:
#   - examples/forward               (tun2, no GRO/GSO offload)
#   - examples/forward-offload-linux (tun-rs, Linux GRO/GSO offload via IFF_VNET_HDR)
#
# Setup: creates a veth pair + network namespace; iperf3 server runs inside
# the namespace, the forward proxy bridges traffic through a TUN device.
#
# Requirements: cargo, iperf3, ip (iproute2), root/CAP_NET_ADMIN
#
# Usage:
#   sudo bash scripts/bench-offload.sh
#
# Run from the root of the netstack-smoltcp repository.

set -euo pipefail

# ── config ────────────────────────────────────────────────────────────────────
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NS=bench
VETH_HOST=veth-host
VETH_NS=veth-bench
HOST_IP=172.19.0.1
NS_IP=172.19.0.2
PREFIX=24
TUN_NAME=utun8
TUN_IP=10.10.10.2
IPERF_PORT=5201
DURATION=15
STREAMS=2

# ── helpers ───────────────────────────────────────────────────────────────────
die()     { echo "ERROR: $*" >&2; exit 1; }
require() { command -v "$1" &>/dev/null || die "'$1' not found"; }

cleanup() {
    pkill -f "forward-" 2>/dev/null || true
    ip netns exec "$NS" pkill iperf3 2>/dev/null || true
    ip route del "${NS_IP}/32" dev "$TUN_NAME" 2>/dev/null || true
    ip tuntap del dev "$TUN_NAME" mode tun 2>/dev/null || true
    ip link del "$VETH_HOST" 2>/dev/null || true
    ip netns del "$NS" 2>/dev/null || true
}
trap cleanup EXIT

# ── preflight ─────────────────────────────────────────────────────────────────
require cargo
require iperf3
require ip
[[ $EUID -eq 0 ]] || die "run as root (needs CAP_NET_ADMIN for TUN + netns)"
[[ -f "$REPO_DIR/Cargo.toml" ]] || die "run from the netstack-smoltcp repo root"
grep -q 'name = "netstack-smoltcp"' "$REPO_DIR/Cargo.toml" \
    || die "Cargo.toml does not look like netstack-smoltcp"

# ── network setup ─────────────────────────────────────────────────────────────
echo "[net] setting up namespace '$NS' and veth pair..."
cleanup 2>/dev/null || true
sleep 0.5

ip netns add "$NS"
ip link add "$VETH_HOST" type veth peer name "$VETH_NS"
ip link set "$VETH_NS" netns "$NS"
ip addr add "${HOST_IP}/${PREFIX}" dev "$VETH_HOST"
ip link set "$VETH_HOST" up
ip netns exec "$NS" ip addr add "${NS_IP}/${PREFIX}" dev "$VETH_NS"
ip netns exec "$NS" ip link set "$VETH_NS" up
ip netns exec "$NS" ip link set lo up
echo "[net] ${HOST_IP} <──veth──> ${NS_IP} (ns:${NS})"

# ── build: forward (tun2, no offload) ────────────────────────────────────────
echo ""
echo "[build] examples/forward (tun2, no GRO/GSO offload)..."
(
    cd "$REPO_DIR"
    cargo build --example forward --release --quiet
    cp target/release/examples/forward /tmp/forward-tun2
)
echo "[build] done → /tmp/forward-tun2"

# ── build: forward-offload-linux (tun-rs, GRO/GSO offload) ───────────────────
echo ""
echo "[build] examples/forward-offload-linux (tun-rs, GRO/GSO offload)..."
(
    cd "$REPO_DIR"
    cargo build --example forward-offload-linux --release --quiet
    cp target/release/examples/forward-offload-linux /tmp/forward-tun-rs
)
echo "[build] done → /tmp/forward-tun-rs"

# ── benchmark runner ──────────────────────────────────────────────────────────
run_bench() {
    local label="$1" binary="$2"

    # clean any leftover state
    pkill -f "forward-" 2>/dev/null || true
    ip netns exec "$NS" pkill iperf3 2>/dev/null || true
    ip route del "${NS_IP}/32" dev "$TUN_NAME" 2>/dev/null || true
    ip tuntap del dev "$TUN_NAME" mode tun 2>/dev/null || true
    sleep 0.8

    # start iperf3 server inside namespace
    ip netns exec "$NS" iperf3 -s -p "$IPERF_PORT" -D \
        --logfile /tmp/iperf3-bench-server.log

    # start proxy
    "$binary" -i "$VETH_HOST" -n "$TUN_NAME" --log-level warn &
    sleep 2

    ip link show "$TUN_NAME" &>/dev/null \
        || { echo "  [!] TUN not up, skipping"; return 1; }

    # route iperf3 traffic through TUN (more-specific /32 overrides /24 via veth)
    ip route add "${NS_IP}/32" dev "$TUN_NAME"

    echo "  running iperf3: ${STREAMS} streams × ${DURATION}s …"
    local out
    out=$(iperf3 -c "$NS_IP" -p "$IPERF_PORT" \
              -t "$DURATION" -P "$STREAMS" 2>&1)

    local sender receiver
    sender=$(echo "$out"   | grep "SUM.*sender"   | awk '{print $6, $7}')
    receiver=$(echo "$out" | grep "SUM.*receiver" | awk '{print $6, $7}')

    if [[ -z "$sender" ]]; then
        echo "  result: FAILED"
        echo "$out" | tail -5 | sed 's/^/    /'
    else
        printf "  sender:   %s\n" "$sender"
        printf "  receiver: %s\n" "$receiver"
    fi

    pkill -f "forward-" 2>/dev/null || true
    ip netns exec "$NS" pkill iperf3 2>/dev/null || true
    ip route del "${NS_IP}/32" dev "$TUN_NAME" 2>/dev/null || true
    ip tuntap del dev "$TUN_NAME" mode tun 2>/dev/null || true
    sleep 0.8
}

# ── direct baseline ───────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " BASELINE: direct veth (no TUN, no proxy)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
ip netns exec "$NS" pkill iperf3 2>/dev/null || true; sleep 0.3
ip netns exec "$NS" iperf3 -s -p "$IPERF_PORT" -D \
    --logfile /tmp/iperf3-bench-server.log; sleep 0.3
echo "  running iperf3: ${STREAMS} streams × ${DURATION}s …"
baseline_out=$(iperf3 -c "$NS_IP" -p "$IPERF_PORT" \
                   -t "$DURATION" -P "$STREAMS" 2>&1)
echo "$baseline_out" | grep "SUM.*sender"   | awk '{printf "  sender:   %s %s\n", $6, $7}'
echo "$baseline_out" | grep "SUM.*receiver" | awk '{printf "  receiver: %s %s\n", $6, $7}'
ip netns exec "$NS" pkill iperf3 2>/dev/null || true; sleep 0.5

# ── tun2 ─────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " tun2  (main branch — no GRO/GSO offload)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
run_bench "tun2" /tmp/forward-tun2

# ── tun-rs + offload ──────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " tun-rs (patched — GRO/GSO offload via IFF_VNET_HDR)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
run_bench "tun-rs+offload" /tmp/forward-tun-rs

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " done."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"