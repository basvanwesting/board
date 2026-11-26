#!/bin/sh
# Writes the lists in this directory with the old list-file tool, so the
# conversion is tested against the old writer, not against itself.
#
#   git archive 3a9f1ea | tar -x -C "$tmp/src"
#   (cd "$tmp/src" && cargo build --release)
#   tests/fixtures/migrate/make.sh "$tmp/src/target/release/board"
#
# Then what the old tool does not write, as by hand: c2's second body line
# indented by a tab instead of the two spaces, its last by one space, and
# a chore without a check (c6). (A line of one space before text that
# starts with blanks reads as two spaces before one less, so that case is
# not made.) ls.txt is the old tool's output for the lists as they end up.
set -eu
old=$(realpath "$1")
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp/home" BOARD_ROOT="$tmp/data"
mkdir -p "$HOME"
git init -q --bare -b main "$tmp/remote.git"
git clone -q "$tmp/remote.git" "$BOARD_ROOT" 2>/dev/null
git -C "$BOARD_ROOT" config user.name fixture
git -C "$BOARD_ROOT" config user.email fixture@example.invalid
git -C "$BOARD_ROOT" commit -q --allow-empty -m start
git -C "$BOARD_ROOT" push -q origin main
b() { "$old" --board proj "$@"; }

b add chore "Water the plants" --group Home --check "the soil is damp" >/dev/null
printf 'Steps:\n  - open the valve\n    - slowly\nthen close it\n' |
  b add chore "Rotate the key." --group Work --check "login works: run it. Done." --from 126 --body >/dev/null
b add chore "Ship it!" --group Work --check "deployed" --from c7 >/dev/null
b add chore "Is it up?" --group Work --check "status page: green" >/dev/null
b add chore "Sweep the porch" --group Home --check "no leaves" >/dev/null
b move c5 --before c1 >/dev/null

b add backlog "Search by tag" --from 126 >/dev/null
printf 'Why: hard to find.\n  indented once\n' | b add backlog "Export as CSV." --from b41 --body >/dev/null
b add backlog "Dark mode?" >/dev/null
b add backlog "A plain idea" --from c3 >/dev/null
b move b4 --before b1 >/dev/null

cd "$BOARD_ROOT/proj"
awk '
  /^    - open the valve$/ { sub(/^  /, "\t") }
  /^  then close it$/ { sub(/^  /, " ") }
  { print }
  /^- c1 / { print "- c6 Call the plumber" }
' chores.md >chores.new
mv chores.new chores.md
git commit -q -am "by hand"
git push -q origin main
cp chores.md backlog.md "$here/"
b ls chores >"$here/ls.txt"
b ls backlog >>"$here/ls.txt"
