#!/bin/sh
# Boot the provisioned Alpine+X test guest (see PLAN.md milestones 4-6):
#   scripts/gui-vm.sh <disk.qcow2> [port] [extra test-server.sh options...]
# Serial console on tcp:127.0.0.1:4555 for scripted control.
set -eu
disk=$1; shift
port=${1:-5930}; [ $# -gt 0 ] && shift
exec "$(dirname "$0")/test-server.sh" --port "$port" "$@" -- -m 1024 \
    -drive file="$disk",if=virtio -nic user,model=virtio-net-pci \
    -serial tcp:127.0.0.1:4555,server,nowait
