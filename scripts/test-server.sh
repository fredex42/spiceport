#!/bin/sh
# Start a QEMU VM with a SPICE server for testing spiceport.
#
#   scripts/test-server.sh [--password PW] [--port N] [--tls-port N --tls-dir DIR] [-- extra qemu args]
#
# With no disk the guest shows SeaBIOS and then "No bootable device", which
# is enough to exercise the display channel. Pass e.g. `-- -cdrom some.iso`
# to boot something more interesting. Uses KVM when /dev/kvm is usable.
set -eu

PORT=5900
PASSWORD=
TLS_PORT=
TLS_DIR=
while [ $# -gt 0 ]; do
    case "$1" in
        --password) PASSWORD=$2; shift 2 ;;
        --port) PORT=$2; shift 2 ;;
        --tls-port) TLS_PORT=$2; shift 2 ;;
        --tls-dir) TLS_DIR=$2; shift 2 ;;
        --) shift; break ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

SPICE="port=$PORT,addr=127.0.0.1"
EXTRA=
if [ -n "$PASSWORD" ]; then
    SPICE="$SPICE,password-secret=spicepw"
    EXTRA="-object secret,id=spicepw,data=$PASSWORD"
else
    SPICE="$SPICE,disable-ticketing=on"
fi
if [ -n "$TLS_PORT" ]; then
    # TLS_DIR must hold ca-cert.pem, server-cert.pem and server-key.pem
    # (see scripts/gen-test-certs.sh).
    SPICE="$SPICE,tls-port=$TLS_PORT,x509-dir=$TLS_DIR"
fi

ACCEL=tcg
if [ -w /dev/kvm ]; then ACCEL=kvm; fi

# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -machine q35,accel=$ACCEL -m 512 \
    -nodefaults -display none \
    -vga qxl \
    -spice "$SPICE" \
    -device virtio-serial-pci \
    -chardev spicevmc,id=vdagent,name=vdagent \
    -device virtserialport,chardev=vdagent,name=com.redhat.spice.0 \
    -audiodev spice,id=snd0 -device ich9-intel-hda -device hda-duplex,audiodev=snd0 \
    -device qemu-xhci -device usb-tablet \
    $EXTRA "$@"
