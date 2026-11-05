# API notes for the pinned crates

Checked on 2026-11-05 against the published sources of slint 1.18.1, vello 0.11.0, vello_cpu 0.3.0, wgpu-types 30.0.1, rand 0.10.3 and kira 0.12.6: crate archives downloaded from crates.io, read and grepped, never built. Paths below are inside each crate's archive. Re-check after any version bump. Lines marked "inference" were not confirmed on hardware.

## Slint and wgpu

- The bridge is the `unstable-wgpu-30` feature of `slint` (there is also `unstable-wgpu-29`). It enables the module `slint::wgpu_30`, which re-exports the wgpu crate as `slint::wgpu_30::wgpu`, plus `WGPUConfiguration`, `WGPUSettings` and `TextureImportError`. Upstream's example pins `slint = { version = "~1.18", features = ["unstable-wgpu-30"] }` and warns the module "may be removed or changed in future minor releases". (slint-1.18.1/lib.rs:546–596)
- Slint renders with wgpu only when selected: `slint::BackendSelector::new().require_wgpu_30(config).select()?`. `WGPUConfiguration` is `Manual { instance, adapter, device, queue }` (bring your own) or `Automatic(WGPUSettings)` (the default). (i-slint-backend-selector-1.18.1/api.rs:116–131)
- `WGPUSettings::default()` requests `device_required_limits: Limits::downlevel_webgl2_defaults()`; only `max_texture_dimension_*` is raised from the adapter. In wgpu-types 30.0.1 those defaults set the compute-workgroup and storage limits to 0, and wgpu allows exactly the requested limits. Vello targets "WebGPU with the default limits", i.e. `wgpu::Limits::default()`. Inference: Vello's compute pipelines fail on Slint's default device. (i-slint-core-1.18.1/graphics/wgpu_30.rs:66, :266; wgpu-types-30.0.1/src/limits.rs:344–352, :644–657; vello-0.11.0/README.md:187–188)
- With `require_wgpu_30`, `select()` fails when no GPU-backed adapter exists: "WGPU 30.x rendering was required but no GPU-backed WGPU adapter is available for the requested backends. Set SLINT_WGPU_CPU=1 to allow CPU adapters." Any value of `SLINT_WGPU_CPU` lifts the check. Slint avoids `Backends::GL` by default (and Metal off Apple, DX12 off Windows) unless a backend is chosen explicitly, e.g. `WGPU_BACKEND=gl`. Unverified: whether Mesa lavapipe reports as a CPU adapter. (i-slint-backend-selector-1.18.1/api.rs:306–323; i-slint-core-1.18.1/graphics/wgpu_30.rs:152–171, :194–197, :237–240)
- Rendering notifier: `Window::set_rendering_notifier(|state: RenderingState, api: &GraphicsAPI| …)`. `GraphicsAPI::WGPU30 { instance, device, queue }` is `#[non_exhaustive]`, so match it with `..`. `RenderingState` is `RenderingSetup | BeforeRendering | AfterRendering | RenderingTeardown` and carries no timestamp, vsync count or refresh rate. Also available: `Window::request_redraw()` and `slint::invoke_from_event_loop(f: impl FnOnce() + Send + 'static)`. (i-slint-core-1.18.1/api.rs:306–352, :521–535, :1286)
- Texture import: `impl TryFrom<wgpu::Texture> for slint::Image`. The format must be `Rgba8Unorm` or `Rgba8UnormSrgb` and the usage must include `TEXTURE_BINDING` and `RENDER_ATTACHMENT`, else `TextureImportError::{InvalidFormat, InvalidUsage}`. (i-slint-core-1.18.1/graphics/wgpu_30.rs:101–116)
- `AboutSlint` is exported from `std-widgets.slint` in every style, Fluent included. (i-slint-compiler-1.18.1/widgets/common/about-slint.slint:6)
- Slint's own `renderer-vello` feature pulls `vello ^0.10` and `unstable-wgpu-29` (through `i-slint-renderer-anyrender`): it is not the scene renderer, and enabling it adds a second Vello and wgpu.
- slint 1.18.1 declares rust-version 1.92. Its licence expression is `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`.

## Vello

- vello 0.11.0 requires `wgpu ^30.0.0` (optional, through the default features `wgpu` and `wgpu_default`), declares MSRV 1.89, calls itself alpha, needs compute shaders and has no blur or filter effects yet. (vello-0.11.0/README.md:31–34, :48–51, :76)
- `Renderer::new(&Device, RendererOptions)` and `render_to_texture(&Device, &Queue, &Scene, &TextureView, &RenderParams)`; the target must be `Rgba8Unorm` with `STORAGE_BINDING`. With Slint's import rules, the shared scene texture is `Rgba8Unorm` with `STORAGE_BINDING | TEXTURE_BINDING | RENDER_ATTACHMENT`. (vello-0.11.0/src/lib.rs:434, :464–466, :476–483)
- `vello::Scene` is exported without the `wgpu` feature (`Scene::new()`, `Send + Sync`): scenes can be built without a GPU, and a crate can depend on vello with `default-features = false` to avoid compiling wgpu. (vello-0.11.0/src/lib.rs:142–147; src/scene.rs:45–56)
- `RendererOptions::use_cpu` still needs a wgpu device and is "not a recommended configuration": it is no fallback for machines without a GPU. (vello-0.11.0/src/lib.rs:373–382)
- vello_cpu 0.3.0 (same release as vello 0.11.0, MSRV 1.89) has its own API: `RenderContext::new(w, h)`, `set_paint`, `fill_path` / `stroke_path` / `glyph_run`, `flush`, `render(&mut Pixmap, &mut Resources)`. It does not depend on vello and never takes a `vello::Scene`; both share peniko ^0.6.1 and kurbo ^0.13.1. "Complex filter graphs are currently not supported at all and will panic". (vello_cpu-0.3.0/README.md:44–74, :144)
- velato 0.12.0, Vello's Lottie integration, still depends on `vello ^0.10` (wgpu 29).

## rand 0.10.3

- `gen_range` was renamed `random_range` in 0.9. In 0.10 the user-level trait is `RngExt` (`use rand::RngExt;`), while `rand::Rng` is the dyn-safe core trait, so the core's RNG can be injected as `&mut dyn rand::Rng`. (rand-0.10.3/CHANGELOG.md:63, :160; src/rng.rs:22–34)
- `random_range` on integers uses Canon's method, slightly biased unless the `unbiased` feature is on (the README accepts a bias of at most one in 2^48 samples). `Uniform::new(low, high)` returns a `Result`, and its `.sample(rng)` uses Lemire's method, unbiased. (rand-0.10.3/src/distr/uniform_int.rs:61, :149, :185–189; README.md:82–86)
- The OS generator is `rand::rngs::SysRng` (renamed from `OsRng` in 0.10). It implements only `TryRng` and `TryCryptoRng`: `rand::rand_core::UnwrapErr(SysRng)` makes it an `Rng` that panics if the OS source fails. `SeedableRng::from_os_rng` is gone. (rand-0.10.3/CHANGELOG.md:58, :62; src/rngs/mod.rs:118–119; getrandom-0.4.3/src/sys_rng.rs:20–26)
- For seeded tests, `StdRng` and `SmallRng` "may be changed in any release". Portable generators: `Xoshiro256PlusPlus`, `Xoshiro128PlusPlus`, and `ChaCha8Rng` / `ChaCha12Rng` / `ChaCha20Rng` with the `chacha` feature; `SeedableRng::seed_from_u64` exists. (rand-0.10.3/src/rngs/mod.rs:25–52, :107–116)
- Fisher–Yates is `rand::seq::SliceRandom::shuffle(&mut self, rng)`. It calls `random_range` internally, so its tiny default bias only touches the order of the entries that were not drawn. (rand-0.10.3/src/seq/slice.rs:402–421; src/seq/increasing_uniform.rs:53)

## Kira 0.12.6

- Dev profile: Kira's docs list `[profile.dev.package.<crate>] opt-level = 3` for kira, cpal and several symphonia crates, or instead `[profile.dev.package."*"] opt-level = 3`. The per-crate list names `symphonia-format-wav`, which is not in kira 0.12's tree (symphonia 0.6 reads WAV through `symphonia-format-riff`), so Cargo warns about an unmatched spec: use the `"*"` form. (kira-0.12.6/src/lib.rs:158–200)
- Clocks: `AudioManager::add_clock(speed) -> Result<ClockHandle, ResourceLimitReached>`, with `ClockSpeed::{SecondsPerTick, TicksPerSecond, TicksPerMinute}(f64)`. Clocks are created stopped: call `ClockHandle::start()`. `ClockHandle::time()` returns `ClockTime { clock, ticks: u64, fraction: f64 }`. Sounds are scheduled with `StaticSoundData::start_time(StartTime::ClockTime(..))`, where `StartTime` is `Immediate | Delayed(Duration) | ClockTime(ClockTime)`. The backend is `kira::DefaultBackend`. (kira-0.12.6/src/clock.rs:1–50; src/start_time.rs:10–19; src/manager.rs:182–185; src/clock/time.rs:19–28; src/clock/clock_speed.rs:6–13)
- Kira exposes no output-latency API (the word "latency" appears nowhere in the crate), so tick compensation relies on per-device calibration.
- kira 0.12.6 depends on cpal ^0.18.1 and symphonia ^0.6.

## Other tools (latest versions on 2026-11-05)

parley 0.12.0 (MSRV 1.88), directories 6.0.0, cargo-packager 0.11.8 (ships `examples/slint`; its updater is the `cargo-packager-updater` crate, 0.2.3; config goes in `Packager.toml`, `packager.json` or `[package.metadata.packager]`), cargo-about 0.9.3, cargo-deny 0.20.2, proptest 1.12.0 (MSRV 1.88), criterion 0.8.2. Rust edition 2024 has been stable since 1.85.0.

Sources: the crates.io API (`https://crates.io/api/v1/crates/<name>/<version>`), crate archives (`https://static.crates.io/crates/<name>/<name>-<version>.crate`), and raw.githubusercontent.com for Slint's licence texts and cargo-packager's README and examples.
