# UI test harness

Interface claims in this project get settled by **looking at the screen** and by
**driving real input** — a container or a headless run cannot falsify what the
window actually draws or what a pointer does to it. These tools are the smallest
thing that closes both loops when the box has no screenshot or input-synthesis
tooling installed.

```bash
./build.sh          # → ./build/{shot,uiclick,udrag,ukey,uwheel}
```

Everything the harness **writes** — captures, fixtures, anything else — goes to
`/tmp/videoplayer/`. `build.sh` creates it; deleting it is always safe.

| Tool | What it is for |
|---|---|
| `shot out.ppm [x y w h]` | grabs the Wayland output (or a region) as a PPM via `wlr-screencopy`; no path = `/tmp/videoplayer/screen.ppm` |
| `pxstats.py image.ppm [x y w h]` | objective read of a rectangle: lit share, mean/max, colour histogram |
| `uiclick X Y N` | moves the real cursor to (X,Y) and sends N left clicks (`0` = move only) |
| `udrag X1 Y1 X2 Y2 [HOLD_MS]` | a real drag-and-drop gesture, holding at the target so a capture can catch the drop feedback |
| `uwheel X Y N [up] [DELAY_MS]` | N wheel notches at (X,Y) |
| `ukey "text"` | types ASCII (a–z, 0–9, `.`/`-`, `!` = Escape) |

## Coordinates

- Window geometry: `hyprctl clients -j` — `at`/`size` are **logical** pixels.
- `shot`'s region arguments are **logical** too, but the PPM comes back at the
  output's scale: a 1024×576 region on a 1.25× display is a 1280×720 image.
  Convert before comparing any on-screen measurement with a widget size.
- The input tools park the virtual mouse at a corner and then walk to the target
  reading `hyprctl cursorpos` (logical) between steps, so pointer acceleration
  and scaling don't matter. They move the **real** cursor.

## Test media fixture

A silent-tone clip, self-contained and long enough to idle through:

```bash
ffmpeg -y -f lavfi -i testsrc2=duration=40:size=640x360:rate=30 \
  -f lavfi -i sine=frequency=440:duration=40 \
  -c:v libx264 -pix_fmt yuv420p -c:a aac /tmp/videoplayer/harness.mkv
```

For path-handling checks use real clips whose names contain spaces and non-ASCII
characters — generated fixtures never exercise that.

## Worked example — idle auto-hide

```bash
pkill -x video-player                       # GApplication is single-instance
../../target/debug/video-player /tmp/videoplayer/harness.mkv &
hyprctl clients -j                          # window at/size, logical
./build/shot /tmp/videoplayer/up.ppm 512 308 1024 576     # chrome up (just poked)
sleep 4
./build/shot /tmp/videoplayer/hidden.ppm 512 308 1024 576 # retracted: video = window
python3 pxstats.py /tmp/videoplayer/hidden.ppm 0 0 1280 40  # top strip = video, not header
./build/uiclick 700 450 0                   # pointer motion is activity
./build/shot /tmp/videoplayer/back.ppm 512 308 1024 576   # chrome back
```

Convert a PPM to PNG (`ffmpeg -i /tmp/videoplayer/up.ppm /tmp/videoplayer/up.png`)
to read it side by side with the claim being tested; `pxstats.py` is for the
claims that are a number.

## Caveats (each one was learned the expensive way)

- `gapplication action aelum.videoplayer.Player <name>` (name **without** the
  `app.` prefix) drives the app's actions over D-Bus. It is the reliable way to
  change playback, and it deliberately does **not** count as pointer or keyboard
  activity — so it cannot be used to test the idle-hide or any hover rule.
- `ukey` is unreliable on this class of setup: keystrokes drop and reorder. Use
  it only where losing one is harmless; for a specific key, prefer a real
  accel-free path or say in the journal that the path was not exercised.
- The input tools need `/dev/uinput`; `shot` needs `wlr-screencopy`. Both are
  Hyprland/wlroots-side — on a GNOME session `shot` reports
  `compositor has no zwlr_screencopy_manager_v1` and the display-path checks
  have to be made with the desktop's own screenshot tool.
- Raising the window over other windows: `hyprctl eval
  "hl.dsp.window.alter_zorder({mode=\"top\", window=\"address:0x…"})"`.
