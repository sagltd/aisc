//! SIGINT/SIGTERM handling: children are killed and reaped, and critical sections finish first.
use std::fmt;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

static PENDING: AtomicI32 = AtomicI32::new(0);
static DEFERRED: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub(crate) struct Interrupted(pub i32);

impl fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "interrupted by signal {}", self.0)
    }
}
impl std::error::Error for Interrupted {}

extern "C" fn handler(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
}

pub(crate) fn install() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe. No SA_RESTART so
    // blocking calls return early and the flag is observed promptly.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

pub(crate) fn pending() -> Option<i32> {
    match PENDING.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

pub(crate) fn deferred() -> bool {
    DEFERRED.load(Ordering::SeqCst) > 0
}

pub(crate) fn exit_code(signal: i32) -> i32 {
    128 + signal
}

/// While alive, a received signal does not abort child processes; callers exit afterwards.
pub(crate) struct Critical;

impl Critical {
    pub fn enter() -> Self {
        DEFERRED.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for Critical {
    fn drop(&mut self) {
        DEFERRED.fetch_sub(1, Ordering::SeqCst);
    }
}
