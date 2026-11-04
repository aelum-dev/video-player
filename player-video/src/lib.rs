//! Video render sink: a `GLArea` subclass that renders mpv frames into
//! GTK's GL context (the Celluloid pattern).
//!
//! Lifecycle follows `mpv-engine`'s contract:
//! - attach on `realize`, with the GLArea's context made current first
//!   (GTK creates the context during realize but does not make it current),
//! - draw in the `render` vfunc via `render_gl(fbo, w, h, flip_y=true)` —
//!   GTK samples the FBO as a top-left-origin texture, so mpv must flip,
//! - detach in `unrealize` with the context still current.
//!
//! mpv's update callback fires on mpv's render thread. Widgets are not
//! `Send`, so it only sets a shared flag; a main-thread timer turns the
//! flag into `queue_render()`.
//!
//! On the swappable-sink rule: this crate *is* the seam —
//! one widget, one crate, so the GL dependency stays out of `player-core`
//! and a second sink (software, DMA-BUF/GSK) would be a sibling module
//! here. A `RenderSink` trait over one implementation is speculation;
//! introduce it when a second sink actually exists.
//!
//! The GL symbol loader lives in [`gllink`] (pure FFI, no GTK); the
//! widget and its lifecycle live here.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use player_core::MpvEngine;

mod gllink;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct VideoArea {
        pub(super) engine: RefCell<Option<Arc<MpvEngine>>>,
        /// Set by mpv's render thread; consumed by the main-thread timer.
        pub(super) frame_pending: RefCell<Option<Arc<AtomicBool>>>,
        /// Physical framebuffer size from the `resize` vfunc.
        pub(super) size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VideoArea {
        const NAME: &'static str = "MpvVideoArea";
        type Type = super::VideoArea;
        type ParentType = gtk::GLArea;
    }

    impl ObjectImpl for VideoArea {}

    impl WidgetImpl for VideoArea {
        fn realize(&self) {
            self.parent_realize();

            let Some(engine) = self.engine.borrow().clone() else {
                return;
            };
            let Some(frame_pending) = self.frame_pending.borrow().clone() else {
                return;
            };

            // GTK creates the context during realize but does not make it
            // current; mpv probes GL at attach and requires it current.
            let Some(context) = self.obj().context() else {
                tracing::error!("GLArea has no GL context after realize");
                return;
            };
            context.make_current();

            // mpv resolves its GL symbols through this closure for the
            // context's whole lifetime.
            let get_proc_address = Box::new(move |name: &str| gllink::resolve(name));
            // The update callback only flips a flag; the main-thread timer
            // turns it into a redraw request.
            let on_update = move || frame_pending.store(true, Ordering::Relaxed);

            let attached = unsafe {
                engine.inner().attach_gl_render(
                    get_proc_address,
                    mpv_engine::GlRenderOptions::default(),
                    on_update,
                )
            };
            match attached {
                Ok(()) => tracing::info!("mpv GL render context attached"),
                Err(err) => tracing::error!(%err, "mpv GL attach failed"),
            }
        }

        fn unrealize(&self) {
            if let Some(engine) = self.engine.borrow().clone() {
                // mpv's GL objects must be freed with this context current.
                if let Some(context) = self.obj().context() {
                    context.make_current();
                }
                engine.inner().detach_render();
            }
            self.parent_unrealize();
        }
    }

    impl GLAreaImpl for VideoArea {
        fn render(&self, _context: &gtk::gdk::GLContext) -> glib::Propagation {
            gllink::clear_color(0.0, 0.0, 0.0, 1.0);
            let _ = gllink::clear(gllink::COLOR_BUFFER_BIT);

            let Some(engine) = self.engine.borrow().clone() else {
                return glib::Propagation::Stop;
            };
            let (width, height) = self.size.get();
            if width <= 0 || height <= 0 {
                return glib::Propagation::Stop;
            }
            // GTK hands us the id of the framebuffer it will sample as a
            // texture; it does not pass it as an argument.
            let Some(fbo) = gllink::get_integer(gllink::FRAMEBUFFER_BINDING) else {
                return glib::Propagation::Stop;
            };

            if let Err(err) = engine.render_gl(fbo, width, height, true) {
                tracing::warn!(%err, "render_gl failed");
            }
            // `mpv_render_context_render` leaves the FBO binding at 0; put
            // GTK's target back so its own state stays consistent.
            let _ = gllink::bind_framebuffer(fbo);
            glib::Propagation::Stop
        }

        fn resize(&self, width: i32, height: i32) {
            self.size.set((width, height));
            self.parent_resize(width, height);
        }
    }
}

glib::wrapper! {
    /// Full-window mpv video surface.
    pub struct VideoArea(ObjectSubclass<imp::VideoArea>)
        @extends gtk::GLArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VideoArea {
    pub fn new(engine: Arc<MpvEngine>) -> Self {
        let area: VideoArea = glib::Object::new();
        area.set_has_depth_buffer(false);
        // mpv's render API produces black frames on a GLES context with this
        // driver stack; ask GDK for a desktop GL context instead of letting it
        // default to GLES.
        area.set_allowed_apis(gtk::gdk::GLAPI::GL);
        area.add_css_class("video-area");

        let frame_pending = Arc::new(AtomicBool::new(false));
        let imp = area.imp();
        *imp.engine.borrow_mut() = Some(engine);
        *imp.frame_pending.borrow_mut() = Some(frame_pending.clone());

        // Main-thread bridge for mpv's render-thread update signal:
        // consume the flag, ask GTK for a render. ~60 Hz, no-op when idle.
        glib::timeout_add_local(Duration::from_millis(16), {
            let area = area.clone();
            move || {
                if frame_pending.swap(false, Ordering::Relaxed) {
                    area.queue_render();
                }
                glib::ControlFlow::Continue
            }
        });

        area
    }
}
