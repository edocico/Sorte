# Sorte M0 spike

Throwaway binary for milestone M0 (CLAUDE.md, `docs/design.md` § Roadmap): a Vello scene in a Slint window through Slint's wgpu-30 bridge, a spinning wheel with motion blur, ticks scheduled on Kira's clock, colour emoji and complex scripts through Parley, a vello_cpu fallback, and a debug overlay. Nothing here is product code: once the gate passes, port what is needed into the real crates.

**Gate:** 0 dropped frames over 10 s at 144 Hz, crisp text, the same rendering on Windows, macOS and Linux (Wayland and X11). The user judges it on real hardware; the spike reports measurements, not pass or fail.

## Build

- Rust ≥ 1.92 (Slint 1.18.1).
- Linux: the ALSA and fontconfig development files (`sudo dnf install alsa-lib-devel fontconfig-devel` on Fedora, `sudo apt install libasound2-dev libfontconfig-dev` on Debian/Ubuntu).
- Windows and macOS: nothing else.
- Fonts: the labels use the files in `spike/fonts/` (not in git, 7 MB, SIL OFL 1.1). Without them text falls back to system fonts, which differ per OS. Put there `NotoSans[wght].ttf`, `NotoSansMono[wght].ttf`, `NotoSansArabic[wght].ttf`, `NotoSansHebrew[wght].ttf`, `NotoSansDevanagari[wght].ttf`, `NotoSansBengali[wght].ttf`, `NotoSansThai[wght].ttf` and `Noto-COLRv1.ttf` (Noto Color Emoji), with `OFL.txt`. On Fedora they are in `/usr/share/fonts/google-noto-vf/` and `/usr/share/fonts/google-noto-color-emoji-fonts/`; elsewhere from <https://github.com/notofonts> and <https://github.com/googlefonts/noto-emoji>. Copy the same folder to every machine so the comparison is fair. CJK labels use the system's CJK font.

```sh
cd spike
cargo run --release              # the window: Space or the button spins, F12 shows the overlay
cargo run --release -- --help    # every option
cargo test                       # unit tests; the blur test needs a GPU
```

## Measuring the gate

Measure on an idle machine, plugged in: with builds running in the background the same runs miss 30-50 % of refreshes.

**The gate is relative to the platform baseline** (decided 2026-11-05, after an empty Slint window driven by the same frame loop missed about one vsync per 10 s on the M0 machine): `cargo run --release -- --gate 6` runs six pairs of 10 s spins in one window, alternating the baseline (`--null-scene`: the same frame loop, nothing new drawn) and the scene, then prints the misses per minute of each, the scene's excess, an exact one-sided p-value for that excess, and how much of the refresh period our CPU and GPU work take at the 99th percentile. Misses come from wp_presentation where it exists (Wayland), from the clock's count elsewhere. First result, Radeon 780M at 165 Hz: baseline 7.0 per minute, scene 8.0 (excess +1.0, p = 0.50); our CPU p99 1.17 ms (19 % of the period), our GPU p99 4.13 ms (68 %).

1. `cargo run --release -- --bench --overlay --trace bench.csv`: a 10 s spin starts 1.5 s after the window opens; the report goes to stdout and the app exits. Note `DROPPED`, `PRESENTED` (Wayland only), the interval percentiles and `process CPU`.
2. Repeat with:
   - `--entries 1000 --labels numbered` (scale) and `--entries 18 --labels scripts` (emoji and complex scripts);
   - `--no-blur`, to see what the blur pass costs;
   - `--adapter high` and `--adapter low` on machines with two GPUs (`--list-adapters` names them);
   - `--cpu` (the vello_cpu fallback; `--threads N`);
   - `--notifier` against the default `--hook` (see Findings: the notifier makes Slint rebuild its pipelines every frame);
   - `--null-scene` (the same frame loop with nothing new drawn: the platform's own baseline), `--fullscreen`, `--window WxH`;
   - Linux X11: `WAYLAND_DISPLAY= cargo run --release -- --bench`.
3. Compare rendering across machines with offscreen snapshots: `cargo run --release -- --snapshot scripts.png --entries 18 --labels scripts --at 8` (add `--cpu` for the fallback; `--at 0.6` shows the blur).
4. The manual checklist from `docs/design.md` § Testing e verifica: Windows 11, macOS on Apple Silicon, Linux on Wayland (GNOME, KDE) and X11; monitors at 60 and 144 Hz or more; scale 100, 150 and 200 %. Look for: tearing or judder during the spin, text crispness at rest, ticks in sync with the edges passing the pointer (tune "Anticipo audio" until they are, and note the value per device).

What the report means:

- `DROPPED`: refreshes skipped between consecutive frames, judged from frame timestamps on a fitted vsync grid. A heuristic: a late frame may still have been shown on time from the swapchain queue.
- `PRESENTED`: Wayland's `wp_presentation` ground truth: frames the compositor showed, frames it discarded, and vsyncs that passed without a new frame.
- `refresh`: the monitor's nominal rate and the rate fitted from the frames (they differ: see Findings).
- `residual`: how far frame starts sit from the fitted grid, in periods; the clock hides anything under ±0.5.
- `our CPU`: the spike's own per-frame work (building, encoding and submitting the scene); `process CPU`: everything, Slint and the drivers included.

## Layout

| File | Role |
| --- | --- |
| `src/frame.rs` | The drawing contract: display lists, pictures drawn under a transform, the motion-blur request |
| `src/spin.rs` | The closed-form spin of § Spin della ruota: K and n from ω₀ and T, landing, ticks |
| `src/wheel.rs` | The wheel picture (slices, edges, radial labels), pointer, hub, the spin choreography |
| `src/gpu.rs` | Our wgpu device (Vello's limits), Vello rendering into the texture Slint shows, readback |
| `src/blur.rs`, `src/blur.wgsl` | Rotational motion blur and the composite, one compute pass |
| `src/cpu.rs` | The vello_cpu fallback |
| `src/text.rs` | Parley shaping with pinned font fallbacks, ellipsis truncation, a line cache |
| `src/clock.rs` | The vsync-quantized clock: fitted period, phase loop, drops, stalls, VRR |
| `src/audio.rs` | Kira: the synthesized tick, the audio-clock mapping, ticks scheduled 200 ms ahead |
| `src/present.rs` | Wayland `wp_presentation` feedback |
| `src/overlay.rs`, `src/main.rs`, `ui/app.slint` | The F12 overlay, the app, the window |

## Findings

First set: 2026-11-05, Fedora 45, GNOME 51 on Wayland, AMD Radeon 780M (RADV) and NVIDIA RTX 4060 laptop GPU, eDP panel 2560×1600 at 165 Hz (164.70 Hz measured), scale 1.667, release build, 10 s spins, 12 entries unless noted. "Missed" is wp_presentation's count of vsyncs that passed without a new frame.

| Configuration | Missed per 10 s (runs) | Process CPU | Hook → screen p50 |
| --- | --- | --- | --- |
| Radeon 780M, winit hook, 12 entries | 1, 2, 2 | 30–31 % | 24 ms |
| Radeon 780M, motion blur off | 1 | 24 % | 24 ms |
| Radeon 780M, 1000 entries | 1, 6 | 35–37 % | 26 ms |
| Radeon 780M, 18 entries with scripts and emoji | 8 | 32 % | 23 ms |
| RTX 4060 (PRIME), winit hook | 0, 1 | 30–34 % | 24 ms |
| XWayland, Radeon 780M | 1, 2 (clock heuristic: no wp_presentation) | 33–35 % | n/a |
| vello_cpu, 4 threads, Slint on GL FemtoVG | 1, 22 | 87–90 % | 12 ms |
| Radeon 780M, **Slint rendering notifier** | **149, 184** | **103 %** | 15 ms |

These runs had a background load of 1.5–3.5 (this session's own builds and simulations), so they are not the gate measurement: repeat them on an idle machine.

**Where the last missed vsync comes from.** A second set at load 1.5–2 (three runs each unless noted), with our GPU time measured by timestamp queries:

| Configuration | Missed per 10 s | Our GPU time per frame |
| --- | --- | --- |
| Radeon 780M, default | 1, 1, 1 | p50 2.3 ms, p99 3.6–4.0, max 5.6 |
| Radeon 780M, `--null-scene` (our frame loop, nothing new drawn) | 1, 1, 1 | none |
| Radeon 780M, `--no-blur` (2 runs) | 1, 2 | p50 1.15 ms, max 3.2 |
| Radeon 780M, `--window 800x500` (2 runs) | 1, 1 | p50 1.5 ms |
| RTX 4060 (2 runs) | 1, 1 | p50 0.7–0.9 ms |
| Radeon 780M, `--fullscreen` | 1, 2, 2 | p50 2.9 ms; latency drops to 18 ms |
| Radeon 780M, `--fullscreen --null-scene` | 3, 1, 3 | none |

The miss does not come from the wheel's rendering: an empty scene misses as often, on both GPUs, windowed or full screen. It falls at a random moment, about once per run, always with the same shape: one `RedrawRequested` arrives a refresh late (an interval of 12–16 ms), then a 1–2 ms catch-up, while our own work for those frames takes 0.02–1.3 ms. So the swapchain was not freed in time: Slint's draw and present, the driver, or the thread's scheduling. Not tried yet: real-time scheduling (`ulimit -r` is 0 here, so it needs sudo), a non-Slint Wayland client as a baseline, other compositors. The fallback planned for a missed gate (a scene of Slint elements) would face the same baseline. The misses left with the winit hook are isolated: one frame starts 12–17 ms after the previous one while our own work for it takes under 1.2 ms, so the delay comes from outside the frame (compositor, driver or other load). The clock's own count matched wp_presentation's in every run once it re-anchors at the start of each spin.

- **Do not use Slint's rendering notifier for the frame loop.** With one, femtovg-wgpu rebuilds its render pipelines every frame (Slint flushes twice per frame and femtovg evicts the pipelines a flush did not use): 3× the CPU and 9 % of refreshes missed on the iGPU. The spike renders from winit's `RedrawRequested` instead (before Slint draws) and requests the next frame with winit's `request_redraw`. On macOS Slint draws from a CADisplayLink that bypasses `RedrawRequested`: still to be measured there (the spike defaults to the notifier on macOS).
- **No API gives vsync counts or presentation times** (Slint, winit, wgpu). The hook's timestamps sit within ±0.1 period of a fitted grid once the swapchain has refilled; the clock counts refreshes on that grid. Its drop count matched wp_presentation's ground truth (2 and 2).
- **The nominal refresh rate is wrong by 0.19 %** (165.001 Hz reported, 164.70 Hz real): the clock fits the period online, or animated time would drift ~19 ms per 10 s from the audio clock.
- **The first frames after idleness come in a burst** (9.1, 6.7, 4.6, 3.3 ms at a 6.07 ms period) while the swapchain refills; each is still shown on its own refresh. The clock counts one refresh each during a warm-up, then anchors.
- **Display latency is 4–5 refreshes** (24–30 ms) on the wgpu path, 13 ms with GL FemtoVG: Slint fixes Fifo with a frame latency of 2 and offers no way to change it. Ticks must be delayed to match, through the per-device audio calibration.
- **Text**: colour emoji (COLRv1), Arabic, Hebrew, Devanagari, Bengali, Thai, CJK, flags and ZWJ sequences render the same on Vello and vello_cpu, except the Noto 🇮🇹 flag, darker on Vello (its sweep-gradient issue). Labels must use named font families: generic ones pull fontconfig's whole list on Linux.
- **Scale**: 1000 entries cost nothing extra: the wheel is encoded once as a picture and appended under a rotation each frame (0.02 ms of encoding).
- **Motion blur** (one compute pass, up to 64 samples per pixel): 0.6 ms on the RTX 4060, 1.5–2 ms on the 780M at 2560×1600.
- **Audio**: Kira starts a scheduled sound within ±0.33 ms of its target (32-frame chunks, aimed half a chunk late); its clock is only published once per callback (10.67 ms here), so the mapping to `Instant` is learned from observations at random phases. Kira reports no output latency.
