#!/usr/bin/env bash
# Turns the demo's `video` capture into the README's animations (animated
# WebP): 25 frames a second, lossy, with a keyframe at least every 10 frames.
# Without the keyframes lossy WebP drifts: it re-encodes only the parts of a
# frame that changed, and the small errors build up (an explosion's glow stays
# on the walls).
#
#   SMOKE_DEMO_SCRIPT=video SMOKE_DEMO_SIZE=1280x720 SMOKE_DEMO_CAPTURE=clips \
#       cargo run --release --example demo
#   tools/encode_clips.sh clips docs/media
#
# Widths: the hero clip 960 pixels, the clips shown three abreast 640, the rest
# 800. QUALITY sets the WebP quality (default 80). Needs ffmpeg and img2webp
# (libwebp).
set -euo pipefail

src=${1:?usage: encode_clips.sh <frames from SMOKE_DEMO_SCRIPT=video> <output dir>}
out=${2:?usage: encode_clips.sh <frames from SMOKE_DEMO_SCRIPT=video> <output dir>}
quality=${QUALITY:-80}
mkdir -p "$out"

for clip in "$src"/*/; do
    name=$(basename "$clip")
    case $name in
        hero) width=960 ;;
        cutaway | rooms | ledge) width=640 ;;
        *) width=800 ;;
    esac
    frames=$(mktemp -d)
    ffmpeg -loglevel error -framerate 30 -i "$clip/%05d.png" \
        -vf "fps=25,scale=$width:-2:flags=lanczos" "$frames/%05d.png"
    img2webp -loop 0 -lossy -q "$quality" -m 4 -kmin 1 -kmax 10 -d 40 \
        "$frames"/*.png -o "$out/$name.webp" > /dev/null 2>&1
    rm -f "$frames"/*.png
    rmdir "$frames"
    echo "$out/$name.webp: $(( $(wc -c < "$out/$name.webp") / 1024 )) KB"
done
