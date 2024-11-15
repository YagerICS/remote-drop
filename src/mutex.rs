use core::borrow::Borrow;

#[cfg(feature = "embassy")]
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};

pub struct Mutex<T> {
    #[cfg(feature = "embassy")]
    inner: embassy_sync::blocking_mutex::Mutex<CriticalSectionRawMutex, T>,
    #[cfg(feature = "std")]
    inner: std::sync::Mutex<T>,
}
impl<T> Mutex<T> {
    pub fn lock<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        #[cfg(feature = "embassy")]
        {
            return embassy_sync::blocking_mutex::Mutex::lock(&self.inner, f);
        }
        #[cfg(feature = "std")]
        {
            let lock = self.inner.lock().unwrap();
            f(lock.borrow())
        }
    }
    pub const fn new(t: T) -> Self {
        #[cfg(feature = "embassy")]
        {
            let inner = embassy_sync::blocking_mutex::Mutex::new(t);
            return Mutex { inner };
        }
        #[cfg(feature = "std")]
        {
            let inner = std::sync::Mutex::new(t);
            Mutex { inner }
        }
    }
}
