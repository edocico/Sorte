//! Wayland presentation feedback (wp_presentation): for each frame we draw, the
//! compositor reports when it reached the screen and the display's vsync counter
//! (MSC). This is the ground truth for dropped frames on Wayland; elsewhere the spike
//! only has the callback-timestamp heuristic of `clock.rs`.
//!
//! winit doesn't bind wp_presentation, so on Linux we open a second, guest wayland-client
//! connection on winit's own wl_display, bind the global there with an event queue of our
//! own, and ask for feedback on the window's wl_surface. On other targets `new` returns `None`.

use std::time::Instant;

/// One frame the compositor showed.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // only the Wayland code builds it
pub struct Presented {
    /// The number passed to `request` for this frame.
    pub frame: u64,
    /// The display's vertical retrace counter when the frame turned visible (0 if the output
    /// has none).
    pub msc: u64,
    /// When it turned visible, on the same clock as `Instant`. It can be a little later than
    /// the moment `poll` returns it (up to 0.36 ms seen on GNOME): the compositor may send the
    /// event during the vertical blank, before the frame's scanout starts.
    pub time: Instant,
    /// The output's refresh period in nanoseconds as the compositor reports it
    /// (0 = unknown or VRR).
    pub refresh_ns: u32,
    /// wp_presentation_feedback kind flags (VSYNC, HW_CLOCK, HW_COMPLETION, ZERO_COPY).
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Feedback {
    Presented(Presented),
    /// The frame never reached the screen (replaced by a newer one before a vsync, or its
    /// surface went away).
    Discarded {
        frame: u64,
    },
}

#[cfg(target_os = "linux")]
pub use wayland::PresentationFeedback;

#[cfg(not(target_os = "linux"))]
pub use unsupported::PresentationFeedback;

#[cfg(target_os = "linux")]
mod wayland {
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use slint::winit_030::winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    use slint::winit_030::winit::window::Window;
    use wayland_backend::client::{Backend, ObjectId, WaylandError};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
    use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
        self, WpPresentationFeedback,
    };

    use super::{Feedback, Presented};

    /// wp_presentation feedback for one winit window.
    ///
    /// It uses winit's wl_display without keeping it alive, so drop it before the event loop
    /// goes away: don't keep it in a `thread_local!` or a static. It stops for good, after one
    /// log line, when the connection fails or winit destroys the window's surface.
    pub struct PresentationFeedback {
        conn: Connection,
        queue: EventQueue<State>,
        surface: WlSurface,
        presentation: WpPresentation,
        state: State,
        /// Set by the first error, which was logged; after it every call does nothing.
        dead: bool,
    }

    impl PresentationFeedback {
        /// Binds wp_presentation on the window's own Wayland connection. `None` on X11
        /// (XWayland included), or when the compositor lacks the protocol.
        pub fn new(window: &Window) -> Option<Self> {
            let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
                return None;
            };
            let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: winit's wl_display lives as long as its event loop, and the caller drops
            // this object before that (see the type's docs). A guest backend never closes it.
            let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
            let conn = Connection::from_backend(backend);
            // SAFETY: a live wl_surface proxy on that display. winit made it with wayland-client
            // too, so once winit destroys it our handle reads as dead rather than dangling.
            let id = unsafe {
                ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast())
            };
            let surface = WlSurface::from_id(&conn, id.ok()?).ok()?;
            // Only a proxy managed by this wayland-backend shares winit's liveness flag;
            // without it is_alive() would never turn false and a destroyed surface would
            // reach libwayland (possible if winit and we ever resolve different versions).
            if surface.object_data().is_none() {
                return None;
            }

            // Our own queue: dispatching it never runs winit's handlers.
            let mut queue = conn.new_event_queue();
            let mut state = State::default();
            conn.display().get_registry(&queue.handle(), ());
            // The first roundtrip lists the globals (the registry handler binds wp_presentation),
            // the second brings the clock_id event the compositor sends in reply to the bind.
            for _ in 0..2 {
                if let Err(err) = queue.roundtrip(&mut state) {
                    eprintln!("wp_presentation: unavailable ({err})");
                    return None;
                }
            }
            let Some(presentation) = state.presentation.take() else {
                eprintln!("wp_presentation: the compositor doesn't offer it");
                return None;
            };
            let Some(clock) = state.clock.filter(|&clock| clock_ns(clock).is_some()) else {
                eprintln!("wp_presentation: can't read the presentation clock ({:?})", state.clock);
                presentation.destroy();
                return None;
            };
            eprintln!("wp_presentation: bound, presentation clock id {clock}");
            Some(Self { conn, queue, surface, presentation, state, dead: false })
        }

        /// Asks for feedback on the window surface's next commit. Call it right before
        /// Slint draws (and so commits) the frame numbered `frame`. If no commit follows, the
        /// request rides on the next one, and two frame numbers report the same presentation.
        pub fn request(&mut self, frame: u64) {
            if self.dead {
                return;
            }
            // winit and we share the surface's liveness flag, so this sees winit destroy it.
            if !self.surface.is_alive() {
                self.fail("the window's wl_surface was destroyed");
                return;
            }
            // The request travels on the same wl_display ahead of the commit the Vulkan WSI
            // sends when Slint presents, so the compositor ties it to that commit.
            self.presentation.feedback(&self.surface, &self.queue.handle(), frame);
            if let Err(err) = self.conn.flush()
                && !would_block(&err)
            {
                self.fail(err);
            }
        }

        /// The feedback received so far, without blocking.
        pub fn poll(&mut self) -> Vec<Feedback> {
            if self.dead {
                return Vec::new();
            }
            if let Err(err) = self.read() {
                self.fail(err);
            } else if let Err(err) = self.queue.dispatch_pending(&mut self.state) {
                self.fail(err);
            }
            std::mem::take(&mut self.state.received)
        }

        /// Moves whatever the compositor sent from the socket into the event queues.
        /// Events for winit's queue wait there for winit, which notices them before it sleeps.
        fn read(&self) -> Result<(), WaylandError> {
            // None: our queue still holds events read earlier (by winit, say); dispatch first.
            let Some(guard) = self.queue.prepare_read() else {
                return Ok(());
            };
            // libwayland makes a reader wait until every thread that prepared a read has read,
            // and another thread (a WSI or driver one) may have. So read only when the socket
            // has data, which wakes that thread too, and otherwise cancel by dropping the guard.
            let mut fd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, and a zero timeout, so it never waits.
            if unsafe { libc::poll(&mut fd, 1, 0) } < 1 {
                return Ok(());
            }
            match guard.read() {
                Err(err) if !would_block(&err) => Err(err),
                _ => Ok(()),
            }
        }

        /// Stops the feedback for good, with one log line. For a connection error that is
        /// enough: winit shares the connection and reports the failure on its own.
        fn fail(&mut self, err: impl std::fmt::Display) {
            eprintln!("wp_presentation: stopped ({err})");
            self.dead = true;
        }
    }

    impl Drop for PresentationFeedback {
        fn drop(&mut self) {
            // Releases the bound global on the server. Feedback still in flight dies with its
            // last event; the registry has no destructor request.
            self.presentation.destroy();
        }
    }

    /// The target of our queue's dispatch.
    #[derive(Default)]
    struct State {
        /// Bound by the registry handler during `new`.
        presentation: Option<WpPresentation>,
        /// The clock of the presentation timestamps, from the clock_id event during `new`.
        clock: Option<libc::clockid_t>,
        /// Feedback dispatched since the last `poll`.
        received: Vec<Feedback>,
    }

    impl Dispatch<WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            registry: &WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { name, interface, .. } = event
                && interface == WpPresentation::interface().name
                && state.presentation.is_none()
            {
                // Version 1 on purpose: there an output without a fixed refresh rate (VRR) must
                // report refresh 0, as `Presented::refresh_ns` promises; version 2 may report
                // its fastest rate instead, and changes nothing else.
                state.presentation = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }

    impl Dispatch<WpPresentation, ()> for State {
        fn event(
            state: &mut Self,
            _: &WpPresentation,
            event: wp_presentation::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wp_presentation::Event::ClockId { clk_id } = event {
                // The wire carries clockid_t's bits as a uint.
                state.clock = Some(clk_id as libc::clockid_t);
            }
        }
    }

    impl Dispatch<WpPresentationFeedback, u64> for State {
        fn event(
            state: &mut Self,
            _: &WpPresentationFeedback,
            event: wp_presentation_feedback::Event,
            &frame: &u64,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wp_presentation_feedback::Event::Presented {
                    tv_sec_hi,
                    tv_sec_lo,
                    tv_nsec,
                    refresh,
                    seq_hi,
                    seq_lo,
                    flags,
                } => {
                    // `new` succeeds only with a known clock, before any feedback exists.
                    let Some(clock) = state.clock else { return };
                    let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                    let ns = i128::from(secs) * 1_000_000_000 + i128::from(tv_nsec);
                    state.received.push(Feedback::Presented(Presented {
                        frame,
                        msc: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                        time: to_instant(clock, ns),
                        refresh_ns: refresh,
                        flags: flags.into(),
                    }));
                }
                wp_presentation_feedback::Event::Discarded => {
                    state.received.push(Feedback::Discarded { frame });
                }
                // sync_output names the output the frame was synced to: not needed.
                _ => {}
            }
        }
    }

    /// Maps a timestamp on the presentation clock to an `Instant`. Rust's `Instant` reads
    /// CLOCK_MONOTONIC on Linux and compositors normally report that clock, but the protocol
    /// allows any clock_gettime clock (it even suggests one NTP doesn't slew, such as
    /// MONOTONIC_RAW, which drifts against MONOTONIC), so the offset between the two clocks
    /// is measured afresh for every timestamp.
    fn to_instant(clock: libc::clockid_t, ns: i128) -> Instant {
        let now = Instant::now();
        // `new` checked that the clock can be read.
        let Some(now_ns) = clock_ns(clock) else { return now };
        let ago = now_ns - ns;
        let span = Duration::from_nanos(u64::try_from(ago.unsigned_abs()).unwrap_or(u64::MAX));
        let time = if ago >= 0 { now.checked_sub(span) } else { now.checked_add(span) };
        time.unwrap_or(now)
    }

    /// The current time on `clock` in nanoseconds; `None` if clock_gettime rejects the clock.
    fn clock_ns(clock: libc::clockid_t) -> Option<i128> {
        let mut ts = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime only writes `ts`.
        if unsafe { libc::clock_gettime(clock, ts.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: it returned 0, so it filled `ts`.
        let ts = unsafe { ts.assume_init() };
        Some(i128::from(ts.tv_sec) * 1_000_000_000 + i128::from(ts.tv_nsec))
    }

    /// The socket is full or empty for now: not an error.
    fn would_block(err: &WaylandError) -> bool {
        matches!(err, WaylandError::Io(err) if err.kind() == std::io::ErrorKind::WouldBlock)
    }
}

#[cfg(not(target_os = "linux"))]
mod unsupported {
    use super::Feedback;

    /// wp_presentation is Wayland-only: here `new` always returns `None`.
    pub struct PresentationFeedback {}

    impl PresentationFeedback {
        pub fn new(_window: &slint::winit_030::winit::window::Window) -> Option<Self> {
            None
        }

        pub fn request(&mut self, _frame: u64) {}

        pub fn poll(&mut self) -> Vec<Feedback> {
            Vec::new()
        }
    }
}
