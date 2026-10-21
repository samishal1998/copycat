#!/bin/sh
for a in "$@"; do case "$a" in *.a) : > "$a";; esac; done; exit 0
