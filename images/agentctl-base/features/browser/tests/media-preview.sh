#!/usr/bin/env bash
# Tests media-preview's account of when a clip is still, on clips ffmpeg generates.
#
#   media-preview.sh [MEDIA_PREVIEW]
set -euo pipefail

media_preview=${1:-/usr/local/bin/media-preview}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
failures=0

# Writes NAME.mp4: white, with a black box from START to END seconds that, if MOVES, travels.
clip() {
    local name=$1 seconds=$2 start=$3 end=$4 moves=$5
    ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=white:s=640x360:d=${seconds}:r=25" \
        -f lavfi -i "color=black:s=16x24:d=${seconds}:r=25" \
        -filter_complex "[0][1]overlay=x='100+${moves}*300*(t-${start})':y=150:enable='between(t,${start},${end})',format=yuv420p" \
        "$scratch/$name.mp4"
}

# Checks that media-preview's line for NAME.mp4 matching PATTERN reads EXPECTED.
expect() {
    local name=$1 pattern=$2 expected=$3 actual
    actual=$("$media_preview" "$scratch/$name.mp4" | sed -n "s/^  ${pattern}//p")
    if [ "$actual" != "$expected" ]; then
        echo "media-preview $name: expected '${pattern}${expected}', got '${pattern}${actual}'" >&2
        failures=$((failures + 1))
    fi
}

# A screen that changes once is still before and after the change.
clip once 5 2 5 0
expect once 'still long enough to read: ' '0.0 to 1.9 s, 2.0 s to the end'
expect once 'contact sheet: [^(]*' '(evenly spaced frames)'

# A last screen shown for a moment is not readable, however long the comparison runs on.
clip brief-end 3 2.9 3 0
expect brief-end 'still long enough to read: ' '0.0 to 2.8 s'

# Something as small as a pointer travelling is change.
clip pointer 5 2 3 1
expect pointer 'still long enough to read: ' '0.0 to 1.9 s, 3.0 s to the end'

# Three or more still times make a sheet of them.
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=white:s=640x360:d=1:r=25" \
    -f lavfi -i "color=black:s=640x360:d=1:r=25" -f lavfi -i "color=gray:s=640x360:d=1:r=25" \
    -filter_complex '[0][1][2]concat=n=3:v=1,format=yuv420p' "$scratch/three.mp4"
expect three 'contact sheet: [^(]*' '(the last frame of each of those 3 times)'

# A file it cannot read is an error, not a clip that is never still.
echo 'not a video' >"$scratch/broken.mp4"
if "$media_preview" "$scratch/broken.mp4" >/dev/null 2>&1; then
    echo "media-preview broken: succeeded on a file that is not a video" >&2
    failures=$((failures + 1))
fi

[ "$failures" -eq 0 ] || exit 1
echo "media-preview: tests passed"
