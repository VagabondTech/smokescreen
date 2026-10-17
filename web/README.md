# The demo in a browser

The demo is meant for browsers with WebGPU (Chrome and Edge on Windows,
macOS and ChromeOS; Safari and Firefox where WebGPU is turned on). WebGL2
can't run it: the smoke needs compute shaders.

**Status:** it runs in Chrome 155 on an Apple M4 (macOS 26): all three
styles, throwing, shooting and the dial. Safari, Firefox, Edge and other
GPUs haven't been tried yet (see `ROADMAP.md`).

## Playing

Click the scene to look around with the mouse (Esc lets go of it), or hold
the right mouse button. WASD moves, Space and C go up and down. G throws a
smoke grenade, H an HE grenade, the left mouse button shoots, X clears the
smokes and F changes the style; the buttons at the top right do the same.
The help at the bottom left lists the rest.

## Build it yourself

```sh
rustup target add wasm32-unknown-unknown
# wasm-bindgen must be exactly the version in Cargo.lock:
cargo install wasm-bindgen-cli --locked --version "$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p')"

cargo build --profile web --example demo --target wasm32-unknown-unknown --features webgpu
wasm-bindgen --no-typescript --target web --out-dir web/pkg --out-name demo \
    target/wasm32-unknown-unknown/web/examples/demo.wasm

# Any static file server works; the page must come from http://localhost or https.
python3 -m http.server --directory web 8000
```

Then open <http://localhost:8000>.

## GitHub Pages

`.github/workflows/web.yml` builds the same thing on every push, and
publishes it to GitHub Pages when run by hand (Actions → web → Run
workflow), at <https://vagabondtech.github.io/smokescreen/>. Turn Pages on
first: Settings → Pages → Source: GitHub Actions.

The browser build is single-threaded, so fills, lighting and voxelising run
on the main thread between frames; the first smokes can hitch a little.
Starting takes about 2.5 s of work on the main thread on an Apple M4, 0.8 s
of it the smoke's noise texture (made on all cores natively). GPU timings in
the HUD are off by default (`SMOKE_DEMO_GPU_TIMINGS` doesn't exist in a
browser).

Chrome doesn't draw a tab while its window is hidden or the screen is
locked, so a demo left in the background doesn't move on.
