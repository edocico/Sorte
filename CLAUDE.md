# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Sorte (provisional name) is an offline, native desktop app for Windows, macOS and Linux that draws a random entry from a list and stages the draw as a frame-by-frame animated "picker": a PickerWheel-style wheel first, then other pickers, starting with a horse race (M3).

This file was written before any code existed. The spec is `docs/design.md`, in Italian: a snapshot of the living design doc linked at its top. `§` below cites its section headings. This file keeps only the rules that are easy to break and the points the doc leaves open.

## Roadmap

Phases run M0 → M4; each closes only when its gate passes, measured with the debug overlay (F12 key) and the test suite. The doc's "MVP" is M1 and "v1" is M2.

| Phase | Gate |
| --- | --- |
| M0 spike: Slint window showing a Vello scene through the wgpu bridge, wheel with motion blur, ticks scheduled with Kira; frame pacing measured on Windows, macOS and Linux (Wayland and X11); Vello CPU fallback tried | 0 dropped frames over 10 s at 144 Hz, crisp text, same rendering on all three OSes |
| M1 wheel MVP | daily use without blocking bugs; statistical and property tests green |
| M2 v1 | parity with all offline PickerWheel features (see Unsettled) |
| M3 horse race, on the same picker contract | no changes to core, persistence or UI; contract tests green |
| M4 1.0 release | — |

- M0 comes before any product code. The doc's NFR and risk tables also put the 1,000-entry benchmark and colour emoji/complex scripts (Parley + Vello) in the spike, and make every NFR threshold a target to confirm there.
- If M0 misses its gate, the fallback already chosen is a scene built from Slint elements, with no wgpu bridge. It is not the Vello CPU renderer (see Invariants).
- Gates are judged on real hardware with the manual checklist (§ Testing e verifica), which a headless session can't run: report your measurements and leave pass or fail, and any switch to the fallback, to the user.

## Commands

No Cargo workspace exists yet; whoever scaffolds it replaces the code block below with the real commands. Planned: one workspace, Rust stable, edition 2024 (Slint 1.18.1 needs Rust ≥ 1.92); CI on GitHub Actions runs fmt, clippy, tests and a debug build on Linux, Windows and macOS for every push.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
cargo test -p sorte-core <test_name>    # a single test
cargo bench                             # criterion
cargo deny check                        # licences and known vulnerabilities
```

- Keep dependencies optimized in the dev profile (`[profile.dev.package."*"] opt-level = 3`): otherwise audio and animation in debug builds aren't representative.
- On tags: LTO release build, packaging with cargo-packager, signing and publication (§ Build, packaging e distribuzione). cargo-about generates the third-party licence list for the About window.

## Architecture

Seven crates. Only `sorte-app` knows Slint and only `sorte-stage` knows wgpu (the bridge strains this: see Unsettled), so core and pickers are tested without a GPU or UI.

| Crate | Role |
| --- | --- |
| `sorte-core` | decides the outcome (weighted draw, ranking); lists, result actions, history |
| `sorte-pickers` | wheel and race: layout and the choreography that lands on the outcome; the shared contract test suite |
| `sorte-anim` | easing curves, monotone splines, springs, vsync-quantized time; used by the pickers |
| `sorte-stage` | Vello on wgpu into a texture; motion blur; debug overlay |
| `sorte-app` | Slint UI and controller: state, undo/redo, the spin cycle |
| `sorte-audio` | Kira: ticks and sounds scheduled ahead on the audio clock |
| `sorte-store` | atomic JSON, assets by hash, import/export |

- One draw: the controller freezes the list → `sorte-core` decides the outcome → the picker turns it into a `Choreography` → the frame loop renders it until it ends → the controller shows the result, applies the chosen action and saves through `sorte-store`. `sorte-audio` schedules the choreography's events.
- Frame loop: Slint's rendering notifier fires before the window draws (`RenderingState::BeforeRendering`) → t from the vsync-quantized clock → the choreography draws its state at t → `sorte-stage` renders it with Vello into a texture on the wgpu device it shares with Slint, which composites it → another frame is requested only while a choreography runs; at rest nothing draws.
- Threads: the UI thread runs events, the controller and all scene and UI rendering (wgpu stays on one thread); Kira has its own audio thread; saves and imports run on an I/O thread and report back through `slint::invoke_from_event_loop`.

## Invariants

Fairness (§ Algoritmi chiave)
- Outcome first, staging second. Only `sorte-core` chooses, using the OS generator behind an RNG trait (fixed seed in tests). Duration, speed, picker type, a mouse fling (it only yields a `SpinRequest`) or a manual stop never change the result.
- Weights are integers 1..=1000 (default 1) so the arithmetic is exact: draw an unbiased uniform integer r in [0, W), W = total weight of the visible (`enabled`) entries, then binary-search for the first cumulative sum above r.
- Ranking pickers: core puts the chosen entry first ("first wins") or last ("last wins") and Fisher–Yates-shuffles the rest, so the odds are the same under both rules. Never derive a winner from a simulated race.
- Visual randomness (landing point, horse pacing) uses a separate cosmetic generator with its own seed (`CosmeticRng`), never the outcome RNG.

Time and choreography (§ Modello a plugin dei picker, § Qualità delle animazioni)
- `Choreography::render(frame, t)` computes the whole state from t alone, springs and camera included: never accumulate per-frame deltas; the same t gives the same frame at any refresh rate.
- t advances in whole monitor periods by counting vsyncs, not by measuring frame intervals; under VRR/ProMotion use the filtered timestamp.
- Every event (start, ticks, arrival) is known up front through `events()` (wheel ticks in closed form). Audio schedules them on Kira's clock 200 ms ahead, offset by a per-device latency calibration (Kira 0.12 has no latency API), so a tick sounds within 10 ms of its slice edge passing the pointer. Never trigger a sound because a frame noticed a slice edge.
- The wheel lands inside the chosen slice, 15–85% of the way across it; the settle phase (the pointer swinging on a damped spring) can't reach another slice; `replan_stop` (manual stop) replans from the current state to the same landing point at matching velocity.

Pickers (§ Modello a plugin dei picker)
- Write core, store and UI against the picker contract, never against the wheel: M3 must add the race without touching them.
- A picker's settings UI comes only from `settings_schema()` (it generates the Aspetto panel and validates the list's `picker_settings` JSON); every picker is registered in the app's picker registry and passes the shared contract suite. v1 has no runtime-loaded pickers.

Persistence (§ Modello dati e salvataggio)
- Per-user data dir from the `directories` crate: one self-contained JSON per list (`lists/<uuid>.json`), images as `assets/<sha256>.<ext>`, plus `settings.json`.
- Every write is atomic (temp file, fsync, rename); autosave fires 500 ms after the last edit.
- `ListDoc.schema` versions the format: documents migrate in memory to the latest version on open, with one test per step.
- `DrawRecord` copies the entry text so history survives deletion; each list's history keeps its last 1,000 draws.

Dependencies, privacy, licensing
- No network connections or telemetry; review new dependencies for this. No update check either (an optional updater is only to be evaluated after 1.0). One executable, no external runtime; scene fonts embedded.
- Upgrade Slint, Vello and wgpu as one unit: Slint's wgpu bridge (feature `unstable-wgpu-30`) may change in any minor release, and Vello 0.11 requires wgpu 30. Pin Slint with a tilde requirement (now `~1.18`; a bare `1.18` accepts 1.19), and update every version this file names when you bump. Upgrade Vello, which is alpha, only when the reference-image suite passes.
- Vello needs compute shaders: detect at startup and fall back to Vello CPU behind a Sorte-owned drawing abstraction, one backend per renderer. Motion blur is our own wgpu pass; Vello has no blur or filters.
- Slint's Royalty-free licence needs the AboutSlint widget in an About window reachable from the top-level menu (alternatives: the Slint badge on the download page, or GPLv3). Sorte's own licence is undecided: don't add a LICENSE file or a `license` field, and don't settle cargo-deny's licence allow-list alone, since slint is `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` and whichever it accepts picks Slint's licence path. Reuse none of PickerWheel's name, logo, texts or graphics.

UI (§ Esperienza utente)
- Every change to entries and settings goes through undo/redo. Everything works from the keyboard: the result window with Tab, Enter and Esc; Space draws only outside text fields; Cmd replaces Ctrl on macOS.
- v1 ships Italian and English (F10), so UI text goes through Slint's `@tr()` from M1 on.
- Colours, radii, motion durations and easing curves are design tokens; every Slint animation takes its duration and curve from them. The style starts from Slint's Fluent theme.

## Tests (§ Testing e verifica)

- Statistical (gates M1): a chi-square test of each entry's frequency against weight ÷ W over 10⁶ draws, with a fixed seed and fixed thresholds.
- Property (`proptest`, gates M1): with random lists, weights, start angles and durations, the wheel stops inside the drawn slice, away from its edges; in the race the arrival order is the ranking and the curves are monotone.
- Contract: every picker passes the shared suite in `sorte-pickers`: for random outcomes its choreography ends on the outcome, within its declared duration, with events in order.
- Reference images: the scene rendered offscreen at fixed instants and compared within a tolerance; in CI on a software GPU (e.g. Mesa lavapipe) or Vello CPU.

## Verified API notes

Checked on 2026-11-05 against the sources of slint 1.18.1, vello 0.11.0, vello_cpu 0.3.0, rand 0.10.3 and kira 0.12.6. Where the doc names an API differently, trust these.
- rand 0.10 has no `gen_range` (the doc's name): it is `random_range` on `rand::RngExt`, and on integers it is slightly biased by default. For the draw use `Uniform::new(0, w)?` with `.sample(&mut rng)` (unbiased) or enable rand's `unbiased` feature. The OS generator is `rand::rngs::SysRng` (formerly `OsRng`) and is fallible: wrap it in `rand::rand_core::UnwrapErr` (which panics if the OS source fails) or handle its errors. Seeded tests need a named portable PRNG (`ChaCha8Rng` with the `chacha` feature, or `Xoshiro256PlusPlus`); `StdRng` and `SmallRng` may change between releases.
- Slint renders with wgpu only when selected with `BackendSelector::new().require_wgpu_30(config).select()`. The default `WGPUSettings` requests `Limits::downlevel_webgl2_defaults()`, whose compute and storage limits are zero, so Vello likely can't run on Slint's default device: raise `device_required_limits` to the WebGPU defaults Vello targets (`wgpu::Limits::default()`) or pass your own device with `WGPUConfiguration::Manual` (confirm in M0).
- `select()` fails at startup when no GPU-backed adapter is found: Slint skips GL by default, so a GL-only GPU counts as none, like a VM with only a CPU adapter; `SLINT_WGPU_CPU` lifts the CPU-adapter check. The no-GPU fallback must catch that error, not only a device without compute, and CI that opens the Slint window on a software adapter may need the variable.
- Don't enable Slint's `renderer-vello` feature: it pulls Vello 0.10 on wgpu 29.
- The shared scene texture must be `Rgba8Unorm` with `STORAGE_BINDING`, `TEXTURE_BINDING` and `RENDER_ATTACHMENT`: Vello writes it as a storage texture, `Image::try_from` needs the other two.
- `vello_cpu` has its own API (`RenderContext`, `Pixmap`) and can't render a `vello::Scene`; vello's `RendererOptions::use_cpu` still needs a wgpu device, so it is no fallback. A `vello::Scene` can be built without a GPU, and without compiling wgpu if vello is added with `default-features = false`.
- Kira clocks are created stopped: `add_clock`, then `ClockHandle::start()`; schedule sounds with `StartTime::ClockTime`.

## Unsettled in the design

Don't pick a side silently: settle these with the user, record the decision in `docs/design.md` (in Italian, like the rest of it), tell the user so the living doc follows, and drop the line here. Roughly in the order they come up:
- M0 spike: whether its code joins the workspace or stays throwaway; if M0 misses its gate, what replaces `sorte-stage` and the Vello scene.
- Bridge ownership: the Slint↔wgpu hand-off (Slint's device in, `Image::try_from` out) needs Slint and wgpu types in one crate, and `unstable-wgpu-30` sits on `sorte-app`'s slint dependency, so neither "only `sorte-stage` knows wgpu" nor the doc's "upgrade only in `sorte-stage`" (§ Stack tecnologico e decisione) can hold literally.
- Vsync source: Slint's rendering notifier passes no timestamp, vsync count or refresh rate, and the doc names no source for them or for the VRR filtered timestamp; finding one is M0 work.
- `Frame` (what `render` draws into): where it lives, and how a choreography asks for motion blur or runs on Vello CPU.
- Cargo dependency graph (the crate map shows data flow, plus pickers → anim) and type ownership: `ListDoc`, `Entry` and `DrawRecord` in core or store; which crate defines `Picker`, `Choreography` and `Outcome`.
- Picker crates: one `sorte-pickers` crate holding wheel and race (the diagram) or one crate per picker under `pickers/` (§ Modello a plugin dei picker, "Aggiungere un picker").
- Wheel kinematics (§ Spin della ruota; there t is the landing angle and τ the time): no mapping from speed level (1–10) to the initial angular speed ω₀; with the easing exponent n limited to [2.5, 6], every landing angle is reachable only if ω₀·T ≳ 27 rad (T = duration), so slow or short spins, including Reduce motion's < 1 s, may have no solution; the optional wind-up and short acceleration are in neither θ(τ), which decelerates from ω₀ at τ = 0, nor the tick formula.
- Idle animation (`render_idle`, the v1 idle rotation) vs "request frames only while a choreography runs".
- About window: the doc's main window has no menu to reach it from (see the Slint licence rule).
- M2 gate: parity with all offline PickerWheel features, yet § Riferimento: PickerWheel defers read-aloud and the logo, banner and custom spin button to after v1.
- Gaps the M3 gate will expose: no phase before M3 builds core's ranking draw, and nothing carries the first/last rule to core (`OutcomeKind` is only `Single | Ranking`); `SpinRequest` and `ListView` are undefined, so how speed and duration (in `ListSettings`), Reduce motion (a Preferences option: fades instead of large movement) and the wheel's resting angle between spins reach `choreograph` and `render_idle` is open; requirement F1 ("any list, any picker") vs `PickerMeta.entries` (2..=12 for the race); the result overlay shows a single entry, not a ranking.
