# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Sorte (provisional name) is an offline, native desktop app for Windows, macOS and Linux that draws a random entry from a list and stages the draw as a frame-by-frame animated "picker": a PickerWheel-style wheel first, then other pickers, starting with a horse race (M3). It is open source under GPLv3 (`LICENSE`).

The M0 spike is built (`spike/`; its README has the measurements and the gate procedure) and the next step is M1. The spec is `docs/design.md`, in Italian: a snapshot of the living design doc linked at its top, re-exported after the review of 2026-11-05 and updated with the M0 decisions. `§` below cites its section headings. This file keeps the rules that are easy to break and the points still open; `docs/api-notes.md` holds the API details of the pinned crates, most of them verified by building and running the spike.

## Roadmap

Phases run M0 → M4; each closes only when its gate passes, measured with the debug overlay (F12 key) and the test suite. The doc's "MVP" is M1, "v1" is M2 and "1.0" is the M4 release.

| Phase | Gate |
| --- | --- |
| M0 spike: Slint window showing a Vello scene through the wgpu bridge, wheel with motion blur, ticks scheduled with Kira; frame pacing measured on Windows, macOS and Linux (Wayland and X11); Vello CPU fallback tried; 1,000-entry benchmark; colour emoji and complex scripts in Parley + Vello | on each OS, `--gate 6` at ≥ 144 Hz: at most 1 missed vsync per minute more than the platform baseline (the same frame loop with nothing drawn), no stalls, our CPU and GPU work under 25 % and 75 % of the refresh period at p99; crisp text, same rendering on all three OSes |
| M1 wheel MVP | daily use without blocking bugs; statistical and property tests green |
| M2 v1, including the ranking draw in core | parity with the offline PickerWheel features planned for MVP and v1 |
| M3 horse race, on the same picker contract | the only change outside the race's crate is registering it; contract tests green |
| M4 1.0 release | — |

- The spike is a throwaway binary in `spike/`, outside the product crates: once it passes, port only what is needed into the real crates. The gate is relative because on the M0 machine an empty Slint window driven by the same loop already misses about one vsync per 10 s (decided 2026-11-05).
- If M0 misses its gate, the fallback already chosen is a scene built from Slint elements, with no wgpu bridge; the drawing contract (`Frame`) is then redesigned before M1. It is not the Vello CPU renderer (see Invariants).
- Gates are judged by the user on real hardware with the manual checklist (§ Testing e verifica): report your measurements and leave pass or fail, and any switch to the fallback, to the user.

## Commands

One Cargo workspace: `crates/sorte-*` and `pickers/*`, Rust stable, edition 2024 (Slint 1.18.1 needs Rust ≥ 1.92); `spike/` is excluded and builds on its own. CI (`.github/workflows/ci.yml`) runs fmt, clippy with `-D warnings`, tests and a debug build on Linux, Windows and macOS for every push, and cargo-deny.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
cargo test -p sorte-core <test_name>    # a single test
cargo run -p sorte-app                  # the app (binary `sorte`)
cargo bench                             # criterion
cargo deny check                        # licences and known vulnerabilities
cd spike && cargo run --release -- --gate 6   # the M0 gate measurement (see spike/README.md)
```

- Environment: the Vello path needs a GPU with compute shaders and the spike opens a window, so measurements happen on the user's machines; cloud sandboxes have neither GPU nor display, though builds and headless tests run there. On Linux, cpal (Kira's audio backend) needs the ALSA development files (`libasound2-dev` on Debian/Ubuntu); the first build names any other missing -dev package.
- Keep dependencies optimized in the dev profile (`[profile.dev.package."*"] opt-level = 3`): otherwise audio and animation in debug builds aren't representative.
- Pin Slint, Vello and wgpu in `[workspace.dependencies]`, so they are bumped in one place.
- On tags: LTO release build, packaging with cargo-packager, signing and publication (§ Build, packaging e distribuzione). cargo-about generates the third-party licence list for the About window.

## Architecture

Seven base crates plus one crate per picker under `pickers/`. Only `sorte-app` knows Slint and only `sorte-stage` renders with wgpu (`sorte-app` touches wgpu types only to hand the device and the scene texture to Slint), so core and pickers are tested without a GPU or UI.

| Crate | Role |
| --- | --- |
| `sorte-core` | decides the outcome (weighted draw, ranking); lists, result actions, history; owns `ListDoc`, `Entry`, `DrawRecord`, `Outcome` |
| `sorte-pickers` | the picker contract (`Picker`, `Choreography`, `Frame`, `SpinRequest`) and the shared contract test suite |
| `pickers/*` | one crate per picker (wheel, race): layout and the choreography that lands on the outcome |
| `sorte-anim` | easing curves, monotone splines, springs, vsync-quantized time |
| `sorte-stage` | creates the wgpu device; renders `Frame` with Vello into a texture, or with vello_cpu; motion blur; debug overlay |
| `sorte-app` | Slint UI and controller: state, undo/redo, the spin cycle, the picker registry |
| `sorte-audio` | Kira: ticks and sounds scheduled ahead on the audio clock |
| `sorte-store` | files, atomic writes, migrations, assets by hash, import/export |

- Dependencies: `sorte-core` and `sorte-anim` depend on no workspace crate; `sorte-pickers` uses core and anim; the `pickers/*` crates use `sorte-pickers`, core and anim; `sorte-stage` uses `sorte-pickers` (for `Frame`); `sorte-store` uses core; `sorte-audio` uses `sorte-pickers` (for the events); `sorte-app` uses all of them.
- One draw: the controller freezes the list → `sorte-core` decides the outcome (the controller passes the first/last rule with the draw request) → the picker gets the `Outcome`, a `SpinRequest` and a `CosmeticRng` and returns a `Choreography` → the frame loop renders it until it ends → the controller keeps its rest state (`Choreography::rest()`), shows the result, applies the chosen action and saves through `sorte-store`. `sorte-audio` schedules the choreography's events.
- Frame loop: winit's `RedrawRequested` reaches `slint::Window::on_winit_window_event` (feature `unstable-winit-030`) just before Slint draws → t from the vsync-quantized clock → the choreography draws its state at t through `Frame` → `sorte-stage` renders it into a texture on the device it shares with Slint, which composites it → another frame is requested, with winit's `request_redraw` (Slint's is swallowed there), only while an animation runs (a choreography or the idle rotation); at rest nothing draws. Never use Slint's rendering notifier on Windows and Linux: with femtovg-wgpu it rebuilds the pipelines every frame (M0: 3× the CPU, 9–11 % of vsyncs missed). On macOS Slint draws from a CADisplayLink that bypasses `RedrawRequested`, so the notifier stays there until measured.
- Threads: the UI thread runs events, the controller and all scene and UI rendering (wgpu stays on one thread); Kira has its own audio thread; saves, imports and exports run on an I/O thread and report back through `slint::invoke_from_event_loop`. A PNG export is drawn offscreen by `sorte-stage` on the UI thread and written by `sorte-store` on the I/O thread.

## Invariants

Fairness (§ Algoritmi chiave)
- Outcome first, staging second. Only `sorte-core` chooses, using the OS generator behind an RNG trait (fixed seed in tests). Duration, speed, picker type, a mouse fling (it only yields a release velocity) or a manual stop never change the result.
- Weights are integers 1..=1000 (default 1) so the arithmetic is exact: draw an unbiased uniform integer r in [0, W) with `Uniform::new(0, W)`, W = total weight of the visible (`enabled`) entries, then binary-search for the first cumulative sum above r.
- Ranking pickers: core puts the chosen entry first ("first wins") or last ("last wins") and Fisher–Yates-shuffles the rest, so the odds are the same under both rules. Never derive a winner from a simulated race.
- Visual randomness (landing point, horse pacing) uses a separate cosmetic generator with its own seed (`CosmeticRng`), never the outcome RNG.

Time and choreography (§ Modello a plugin dei picker, § Qualità delle animazioni)
- `Choreography::render(frame, t)` computes the whole state from t alone, springs and camera included: never accumulate per-frame deltas; the same t gives the same frame at any refresh rate.
- t advances in whole monitor periods by counting vsyncs on a grid fitted to the frame start times, not by measuring frame intervals; under VRR/ProMotion use the filtered timestamp. Fit the period online (the nominal rate was 0.19 % off on the M0 machine), accumulate t (never vsync × period, so a refined period can't make it jump), and re-anchor at the start of each animation: the first frames after idleness come in a burst while the swapchain refills. No API gives vsync counts or present times; Wayland's `wp_presentation` is the only ground truth, for measuring.
- Every event (start, ticks, arrival) is known up front through `events()` (wheel ticks in closed form, per segment). Audio schedules them on Kira's clock 200 ms ahead, offset by a per-device latency calibration (Kira 0.12 has no latency API), so a tick sounds within 10 ms of its slice edge passing the pointer. Never trigger a sound because a frame noticed a slice edge. A manual stop cancels the ticks already queued after the stop.
- The wheel lands at φ, inside the chosen slice and 15–85% of the way across it (§ Spin della ruota calls the landing angle φ and time τ); the settle phase (the pointer swinging on a damped spring) can't reach another slice; `replan_stop` replans from the current state to the same φ at matching velocity.
- Duration fixes T; the speed level (1–10) picks ω₀ from a table, nudged to the nearest feasible value when no integer K gives an easing exponent n in [2.5, 6]. A fling's T is clamped to 1–30 s; the optional wind-up and acceleration are short segments inside T.
- Reduce motion reaches pickers through `SpinRequest`: choreographies under 1 s, fades instead of large movement (the wheel doesn't spin, it fades to the result).

Pickers stay pluggable (§ Modello a plugin dei picker)
- Write core, store and UI against the picker contract, never against the wheel: M3 must add the race by touching only its own crate and the registry in `sorte-app`.
- A picker's settings UI comes only from `settings_schema()` (it generates the Aspetto panel and validates the list's `picker_settings` JSON; the wheel's speed and mystery mode live there). Every picker crate runs the shared contract suite from `sorte-pickers` in its own tests. v1 has no runtime-loaded pickers.
- Ctrl+1…9 follow the registry's order. Pickers that don't accept the list's entry count stay listed but disabled, with the reason. The result overlay shows a ranking for ranking pickers.

Persistence (§ Modello dati e salvataggio)
- Per-user data dir from the `directories` crate: one self-contained JSON per list (`lists/<uuid>.json`), images and custom sounds as `assets/<sha256>.<ext>`, plus `settings.json`. A user-chosen data dir is recorded in a small file in the OS config dir, outside the data dir.
- Every write is atomic (temp file, fsync, rename); autosave fires 500 ms after the last edit.
- `ListDoc.schema` versions the format: documents migrate in memory to the latest version on open, with one test per step.
- `DrawRecord` copies the entry text so history survives deletion; each list's history keeps its last 1,000 draws. Closing the result with Esc records `Done`.

Dependencies, privacy, licensing
- No network connections or telemetry; review new dependencies for this. No update check either (an optional updater is only to be evaluated after 1.0). One executable, no external runtime; scene fonts embedded.
- Upgrade Slint, Vello and wgpu as one unit: Slint's wgpu bridge (feature `unstable-wgpu-30`) may change in any minor release, and Vello 0.11 requires wgpu 30. Pin Slint with a tilde requirement (now `~1.18`; a bare `1.18` accepts 1.19), and update every version this file names when you bump. Upgrade Vello, which is alpha, only when the reference-image suite passes.
- Vello needs compute shaders: at startup, fall back to vello_cpu behind `Frame` (one backend per renderer). Motion blur is our own wgpu pass, requested through `Frame` with the centre, the radius of the moving content and the angular velocity; it is skipped on vello_cpu.
- Licence: every crate declares `license = "GPL-3.0-only"`, and cargo-deny allows only GPLv3-compatible licences (it takes Slint under `GPL-3.0-only`). AboutSlint stays in the About window as a courtesy; GPLv3 doesn't require it. Reuse none of PickerWheel's name, logo, texts or graphics.

UI (§ Esperienza utente)
- Every change to entries and settings goes through undo/redo. Everything works from the keyboard: the result overlay with Tab, Enter and Esc; Space draws only outside text fields; Cmd replaces Ctrl on macOS. The top bar's menu holds Preferenze and Informazioni (the app menu on macOS).
- v1 ships Italian and English (F10), so UI text goes through Slint's `@tr()` from M1 on.
- Colours, radii, motion durations and easing curves are design tokens; every Slint animation takes its duration and curve from them. The style starts from Slint's Fluent theme.

## Tests (§ Testing e verifica)

- Statistical (gates M1): a chi-square test of each entry's frequency against weight ÷ W over 10⁶ draws, with a fixed seed and fixed thresholds.
- Property (`proptest`, gates M1): with random lists, weights, start angles and durations, the wheel stops inside the drawn slice, away from its edges; in the race the arrival order is the ranking and the curves are monotone.
- Contract: each picker crate runs the shared suite from `sorte-pickers`: for random outcomes its choreography ends on the outcome, within its declared duration, with events in order.
- Reference images: the scene rendered offscreen at fixed instants and compared within a tolerance; in CI on a software GPU (e.g. Mesa lavapipe) or vello_cpu.

## Verified API notes

Checked on 2026-11-05 against the sources of slint 1.18.1, vello 0.11.0, vello_cpu 0.3.0, rand 0.10.3 and kira 0.12.6, then built and run in the M0 spike (with wgpu 30.0.1, parley 0.12.0, winit 0.30.13); signatures and evidence are in `docs/api-notes.md`.
- Import the scene texture as `Rgba8Unorm`, straight alpha (what Vello writes): FemtoVG-wgpu shows `Rgba8UnormSrgb` too dark. Create the device with `wgpu::Limits::default().using_resolution(adapter.limits())`; check `device_type != Cpu` yourself (with `Manual`, `select()` never looks at your adapter); call `queue.submit([])` before `select()` or debug builds abort at exit.
- Vello re-expands colour emoji on the CPU at every `draw_glyphs`: encode static content once into a retained `Scene` and `append` it with a transform each frame. `Renderer::new` returns `Ok` even when its pipelines failed: check the `COMPUTE_SHADERS` downlevel flag and `Limits::default().check_limits(..)` first.
- Parley: never put a generic font family in a label stack on Linux (fontconfig's whole list outranks the script fallbacks); name the families and pin script fallbacks.
- rand 0.10: `gen_range` is now `random_range` on `rand::RngExt` and is slightly biased on integers, so the draw uses `Uniform::new(0, w)?` with `.sample(&mut rng)` (or rand's `unbiased` feature). The OS generator is `rand::rngs::SysRng` (formerly `OsRng`) and is fallible: wrap it in `rand::rand_core::UnwrapErr` (which panics if the OS source fails) or handle its errors. Seeded tests use a named portable PRNG (`ChaCha8Rng` with the `chacha` feature, or `Xoshiro256PlusPlus`); `StdRng` and `SmallRng` may change between releases.
- Slint renders with wgpu only when selected with `BackendSelector::new().require_wgpu_30(config).select()`, and only with the `renderer-femtovg-wgpu` feature. The default `WGPUSettings` requests `Limits::downlevel_webgl2_defaults()`, whose compute and storage limits are zero (Vello fails on it, confirmed in M0), which is why `sorte-stage` creates the device with the limits Vello needs and `sorte-app` passes it with `WGPUConfiguration::Manual`.
- `select()` fails at startup when no GPU-backed adapter is found: Slint skips GL by default, so a GL-only GPU counts as none, like a VM with only a CPU adapter; `SLINT_WGPU_CPU` lifts the CPU-adapter check. The no-GPU fallback must catch that error, not only a device without compute, and CI that opens the Slint window on a software adapter may need the variable.
- Don't enable Slint's `renderer-vello` feature: it pulls Vello 0.10 on wgpu 29.
- The shared scene texture must be `Rgba8Unorm` with `STORAGE_BINDING`, `TEXTURE_BINDING` and `RENDER_ATTACHMENT`: Vello writes it as a storage texture, `Image::try_from` needs the other two.
- `vello_cpu` has its own API (`RenderContext`, `Pixmap`) and can't render a `vello::Scene`; vello's `RendererOptions::use_cpu` still needs a wgpu device, so it is no fallback. A `vello::Scene` can be built without a GPU, and without compiling wgpu if vello is added with `default-features = false`.
- Kira clocks are created stopped: `add_clock`, then `ClockHandle::start()`; schedule sounds with `StartTime::ClockTime`.

## Still open

Don't settle these alone: ask the user, record the decision in `docs/design.md` (in Italian, like the rest of it), tell them so the living doc follows, and drop the line here.
- Product questions (§ Rischi e domande aperte): final name; distribution beyond direct download and Flathub; user-made WebAssembly pickers (a comment on the living doc still waits for the user's answer); the race's art style; the reference machine for the smoothness target, and manual tests on a 240 Hz monitor and an Intel Mac.
- From M0, for the user: the M0 gate measured on Windows, macOS, X11 and KDE; the macOS frame loop (notifier or hook, measured); the speed table for levels 1–10 (`spike/src/spin.rs` has a placeholder); bundling the Noto fonts (SIL OFL 1.1, 7 MB without CJK) into the binary.
