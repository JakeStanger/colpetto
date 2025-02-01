#![warn(missing_docs)]
#![warn(rustdoc::missing_crate_level_docs)]
#![warn(clippy::all)]
#![warn(clippy::unwrap_used)]
#![warn(clippy::expect_used)]
#![warn(clippy::unimplemented)]
#![warn(clippy::todo)]
#![warn(clippy::dbg_macro)]
#![warn(clippy::panic)]
#![warn(clippy::doc_markdown)]
#![warn(clippy::redundant_clone)]
#![doc = include_str!("../README.md")]

/// Raw bindings generated by bindgen
#[allow(missing_docs)]
#[allow(nonstandard_style)]
#[allow(clippy::all)]
pub mod sys;

use std::{
    ffi::{c_char, c_int, c_void, CStr},
    io, mem,
    os::fd::RawFd,
    ptr::NonNull,
    sync::Arc,
};

use devil::Udev;

mod device;
mod device_group;
mod logger;
mod seat;

pub mod event;

pub use device::*;
pub use device_group::*;
pub use event::Event;
pub use logger::*;
pub use seat::*;

#[cfg(feature = "tokio")]
mod event_stream;
#[cfg(feature = "tokio")]
pub use event_stream::EventStream;

/// Generic error type for libinput
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to resume libinput Context")]
    Resume,
    #[error("Failed to create libinput context")]
    Context,
    #[error("Failed to assign seat")]
    Seat,
    #[error("{0}")]
    IoError(#[from] io::Error),
}

/// Convient alias for colpetto errors
pub type Result<T, E = Error> = std::result::Result<T, E>;

unsafe extern "C" fn open_restricted(
    path: *const c_char,
    flags: c_int,
    user_data: *mut c_void,
) -> c_int {
    let handler = user_data as *const Handler;
    let handler = unsafe { &*handler };

    match (handler.open)(CStr::from_ptr(path), flags) {
        Ok(fd) => fd,
        Err(errno) => errno,
    }
}

unsafe extern "C" fn close_restricted(fd: c_int, user_data: *mut c_void) {
    let handler = user_data as *const Handler;
    let handler = unsafe { &*handler };

    (handler.close)(fd)
}

const INTERFACE: sys::libinput_interface = sys::libinput_interface {
    open_restricted: Some(open_restricted),
    close_restricted: Some(close_restricted),
};

/// The main libinput context
// FIXME: proper docs
pub struct Libinput {
    raw: NonNull<sys::libinput>,
}

#[allow(clippy::type_complexity)] // No point in making a type alias no one will use nor see
struct Handler {
    open: Box<dyn Fn(&CStr, c_int) -> Result<RawFd, c_int> + 'static>,
    close: Box<dyn Fn(c_int) + 'static>,
}

impl Libinput {
    /// Creates a new libinput context. For more information see [`with_logger`](Self::with_logger).
    pub fn new<O, C>(open: O, close: C) -> Result<Self>
    where
        O: Fn(&CStr, c_int) -> Result<RawFd, c_int> + 'static,
        C: Fn(c_int) + 'static,
    {
        Self::with_logger(open, close, None)
    }

    /// Creates a new libinput context with the given logger.
    ///
    /// Internally this will create a new libudev instance and create the internal context with it.
    ///
    /// This function will return an error if either udev or libinput fail to create a context.
    pub fn with_logger<O, C>(open: O, close: C, logger: Logger) -> Result<Self>
    where
        O: Fn(&CStr, c_int) -> Result<RawFd, c_int> + 'static,
        C: Fn(c_int) + 'static,
    {
        let udev = Udev::new()?;

        let handler = Arc::new(Handler {
            open: Box::new(open),
            close: Box::new(close),
        });

        let libinput = unsafe {
            sys::libinput_udev_create_context(
                &INTERFACE,
                Arc::into_raw(handler) as *const _ as _,
                udev.as_raw().cast(),
            )
        };

        if libinput.is_null() {
            return Err(Error::Context);
        }

        logger::setup_logger(libinput, logger);

        Ok(Self {
            raw: unsafe { NonNull::new_unchecked(libinput) },
        })
    }

    /// Returns the raw underlying pointer
    pub fn as_raw(&self) -> *mut sys::libinput {
        self.raw.as_ptr()
    }

    /// libinput keeps a single file descriptor for all events, [`dispatch`](Self::dispatch) should be called only when events are avaiable on this fd
    pub fn get_fd(&self) -> i32 {
        unsafe { sys::libinput_get_fd(self.as_raw()) }
    }

    /// Main event dispatchment function. Reads events of the file descriptors and processes them internally.
    /// Use [`get_event`](Self::get_event) to retrieve the events.
    ///
    /// Dispatching does not necessarily queue libinput events. This function should be called immediately once data is available on the file descriptor returned by [`get_fd`](Self::get_fd).
    /// libinput has a number of timing-sensitive features (e.g. tap-to-click), any delay in calling [`dispatch`](Self::dispatch) may prevent these features from working correctly.
    pub fn dispatch(&self) -> Result<(), Error> {
        unsafe {
            match sys::libinput_dispatch(self.as_raw()) {
                0 => Ok(()),
                e => Err(Error::IoError(io::Error::from_raw_os_error(-e))),
            }
        }
    }

    /// Suspend monitoring for new devices and close existing devices.
    /// This all but terminates libinput but does keep the context valid to be resumed with [`resume`](Self::resume).
    pub fn suspend(&self) {
        unsafe { sys::libinput_suspend(self.as_raw()) }
    }

    /// Resume a suspended libinput context. This re-enables device monitoring and adds existing devices
    pub fn resume(&self) -> Result<(), Error> {
        match unsafe { sys::libinput_resume(self.as_raw()) } {
            0 => Ok(()),
            _ => Err(Error::Resume),
        }
    }

    /// Retrieve the next event from libinput's internal event queue.
    pub fn get_event(&self) -> Option<Event> {
        let event = unsafe { sys::libinput_get_event(self.as_raw()) };

        if event.is_null() {
            return None;
        }

        let event_type = unsafe { sys::libinput_event_get_type(event) };

        if event_type == sys::libinput_event_type::LIBINPUT_EVENT_NONE {
            return None;
        }

        Some(unsafe { Event::from_raw(event, event_type) })
    }

    /// Assigns a seat to this libinput context. After assignment, device changes (additions or removals)
    /// will be reported as events during [`dispatch`](Self::dispatch)
    ///
    /// This function succeeds even when:
    /// - No input devices are currently available on the specified seat
    /// - Available devices fail to open via [`OpenCallback`]
    ///
    /// Devices that lack minimum capabilities to function as a pointer, keyboard, or touch device
    /// are ignored until the next call to [`resume()`](Self::resume). The same applies to
    /// devices that failed to open.
    ///
    /// # Errors
    /// This function may only be called once per context. Subsequent calls will result in an error.
    pub fn udev_assign_seat(&mut self, seat_id: &CStr) -> Result<(), Error> {
        match unsafe { sys::libinput_udev_assign_seat(self.as_raw(), seat_id.as_ptr()) } {
            0 => Ok(()),
            _ => Err(Error::Seat),
        }
    }
}

impl Drop for Libinput {
    fn drop(&mut self) {
        let user_data = unsafe { sys::libinput_get_user_data(self.as_raw()) };

        unsafe {
            sys::libinput_unref(self.as_raw());
            drop(Arc::<Handler>::from_raw(user_data.cast()));
        }
    }
}

impl Clone for Libinput {
    fn clone(&self) -> Self {
        let handler: Arc<Handler> =
            unsafe { Arc::from_raw(sys::libinput_get_user_data(self.as_raw()).cast()) };

        let user_data = handler.clone();

        mem::forget(handler);

        let raw = unsafe { sys::libinput_ref(self.as_raw()) };

        unsafe {
            sys::libinput_set_user_data(raw, Arc::into_raw(user_data) as *const _ as _);
        };

        Self {
            raw: unsafe { NonNull::new_unchecked(raw) },
        }
    }
}

#[cfg(feature = "tokio")]
impl Libinput {
    /// Returns a new `EventStream` that can be used to retrieve events.
    ///
    /// # Panics
    ///
    /// Panics if called outside of a tokio context
    pub fn event_stream(&self) -> Result<EventStream, Error> {
        EventStream::new(self.clone(), self.get_fd())
    }
}

mod macros {
    /// Implements `std::fmt::Debug` on each provided type,
    /// writing `Name(<ptr>)` to obfuscate the pointer field.
    macro_rules! impl_debug {
        ($($name:ident),+) => {
            $(
                impl std::fmt::Debug for $name {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "{}(<ptr>)", stringify!($name))
                    }
                }
            )+
        }
    }

    pub(crate) use impl_debug;
}

macros::impl_debug!(Libinput);
