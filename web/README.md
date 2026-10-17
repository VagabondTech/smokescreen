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

## When it doesn't work

The page says what went wrong instead of sitting on a blank screen: no GPU
for WebGPU, WebGPU errors, a lost GPU device, or a panic. Add `?log` to the
address to see everything the demo logs, with a button to copy it.

The demo's settings go in the address too, to narrow a problem down on a
phone:

| Setting | Effect |
|---|---|
| `?smoke=0` | No smoke at the start: just the scene |
| `?ldr` | No HDR |
| `?msaa=0` | No MSAA |
| `?style=faceted`, `?style=crystal` | Start in that style |

Combine them with `&`: `?log&smoke=0&msaa=0`. They are the demo's
`SMOKE_DEMO_*` environment variables (see the top of `examples/demo.rs`),
except the scripted captures, which write files.

## Build it yourself

```sh
rustup target add wasm32-unknown-unknown
# wasm-bindgen must be exactly the version in Cargo.lock:
cargo install wasm-bindgen-cli --locked --version "$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p')"

cargo build --profile web --example demo --target wasm32-unknown-unknown --features webgpu
wasm-bindgen --no-typescript --remove-name-section --target web --out-dir web/pkg --out-name demo \
    target/wasm32-unknown-unknown/web/examples/demo.wasm

# Any static file server works; the page must come from http://localhost or https.
python3 -m http.server --directory web 8000
```

Then open <http://localhost:8000>.

`--remove-name-section` drops the function names that make stack traces
readable: the module is 44 MB instead of 110 MB (8.7 MB gzipped). Leave it
out when chasing a panic.

## GitHub Pages

`.github/workflows/web.yml` builds the same thing for every pull request
that touches the demo, and publishes `main` to GitHub Pages, at
<https://vagabondtech.github.io/smokescreen/>. Turn Pages on first
(Settings → Pages → Source: GitHub Actions), or the deploy fails.

The browser build is single-threaded, so fills, lighting and voxelising run
on the main thread between frames; the first smokes can hitch a little.
Starting takes about 2.5 s of work on the main thread on an Apple M4, 0.8 s
of it the smoke's noise texture (made on all cores natively). The HUD has no
GPU timings in a browser: Bevy's need timestamps inside command encoders,
which WebGPU doesn't have.

Chrome doesn't draw a tab while its window is hidden or the screen is
locked, so a demo left in the background doesn't move on.
