#!/bin/sh
# One script, four host-initiated channels (picked by the name it is called as). Each reads or
# writes a tar stream on stdin/stdout; socat connects that to vsock.
# The host builds these streams itself, so plain tar -x is fine: GNU tar already drops absolute
# names and `..` parts, and the stream only ever lands on the corn's own disk.
set -eu
user=$(cat /run/popcorn-user 2>/dev/null || echo analyst)
home=/home/$user
# The host sends a plain folder name as the first line for the share channels.
name() { read -r d; case "$d" in */* | .* | '') exit 1 ;; esac; echo "$d"; }
pack() { find . -type f -size -50M -print0 | tar --null --no-recursion -T - -c -f -; }
case "$(basename "$0")" in
popcorn-inbox) tar -x -C "$home/inbox" --no-same-owner && chown -R "$user:$user" "$home/inbox" ;;
popcorn-outbox) cd "$home/outbox" && pack ;;
popcorn-share-in)
  d=$(name) || exit 1
  rm -rf "$home/work/$d" && mkdir -p "$home/work/$d" && tar -x -C "$home/work/$d" --no-same-owner && chown -R "$user:$user" "$home/work/$d" ;;
popcorn-share-out)
  d=$(name) || exit 1
  cd "$home/work/$d" && pack ;;
esac
