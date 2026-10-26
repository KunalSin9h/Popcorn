#!/bin/sh
# One script, four host-initiated channels (picked by the name it is called as). socat connects
# stdin and stdout to vsock.
# The host builds the streams it sends itself, so plain tar -x is fine: GNU tar already drops
# absolute names and `..` parts, and the stream only ever lands on the corn's own disk. What goes
# back to the host is checked there; nothing here needs to be trusted.
set -eu
user=$(cat /run/popcorn-user 2>/dev/null || echo analyst)
home=/home/$user
# The host sends a plain folder name as the first line for the shared-folder channels.
name() { read -r d; case "$d" in */* | .* | '') exit 1 ;; esac; echo "$d"; }
pack() { find . -type f -size -50M -print0 | tar --null --no-recursion -T - -c -f -; }
# Root runs git on the user's repository; the user has sudo in the corn anyway.
g() { git -c safe.directory='*' -c core.hooksPath=/dev/null "$@"; }
case "$(basename "$0")" in
popcorn-inbox) tar -x -C "$home/inbox" --no-same-owner && chown -R "$user:$user" "$home/inbox" ;;
popcorn-outbox) cd "$home/outbox" && pack ;;
popcorn-git-in)
  # A bundle with one commit, refs/popcorn/base: the shared folder, without history.
  d=$(name) || exit 1
  read -r size
  case "$size" in '' | *[!0-9]*) exit 1 ;; esac
  w="$home/work/$d" b="$home/.popcorn-in.bundle"
  head -c "$size" > "$b"
  rm -rf "$w" && g init -q -b work "$w" && cd "$w"
  # Files stay exactly as they are on the host: no line-ending or other conversions.
  echo '* -text -eol -filter -ident -working-tree-encoding' > .git/info/attributes
  g fetch -q "$b" refs/popcorn/base:refs/popcorn/base && g checkout -q -B work refs/popcorn/base
  g config user.name "$user" && g config user.email "$user@popcorn"
  rm -f "$b" && chown -R "$user:$user" "$w" && echo ok ;;
popcorn-git-out)
  # Snapshot the work tree, committed or not, on top of HEAD. Send only what the host lacks.
  d=$(name) || exit 1
  cd "$home/work/$d"
  GIT_INDEX_FILE=$(mktemp) && export GIT_INDEX_FILE
  cp .git/index "$GIT_INDEX_FILE" 2>/dev/null || true
  g add -A >/dev/null
  t=$(g write-tree) && p=$(g rev-parse -q --verify HEAD || true)
  c=$(GIT_AUTHOR_NAME=$user GIT_AUTHOR_EMAIL=$user@popcorn GIT_COMMITTER_NAME=$user GIT_COMMITTER_EMAIL=$user@popcorn g commit-tree "$t" ${p:+-p "$p"} -m "Popcorn snapshot")
  rm -f "$GIT_INDEX_FILE"
  g update-ref refs/popcorn/snapshot "$c"
  g bundle create -q - refs/popcorn/snapshot ^refs/popcorn/base ;;
esac
