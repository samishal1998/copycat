#!/bin/sh
# Check-only stub: produce whatever -o names, empty. Nothing is linked by `cargo check`.
while [ $# -gt 0 ]; do [ "$1" = "-o" ] && { shift; : > "$1"; }; shift; done; exit 0
