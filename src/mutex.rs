use core::borrow::Borrow;

#[cfg(feature = "embassy")]
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};

pub struct Mutex<T> {
    #[cfg(feature = "embassy")]
    inner: embassy_sync::blocking_mutex::Mutex<CriticalSectionRawMutex, T>,
    #[cfg(feature = "std")]
    inner: std::sync::Mutex<T>,
    #[cfg(feature = "loom")]
    inner: loom::sync::Mutex<T>,
}
impl<T> Mutex<T> {
    pub fn lock<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        #[cfg(feature = "embassy")]
        {
            return embassy_sync::blocking_mutex::Mutex::lock(&self.inner, f);
        }
        #[cfg(any(feature = "std", feature = "loom"))]
        {
            let lock = self.inner.lock().unwrap();
            return f(lock.borrow());
        }
    }

    #[cfg(not(feature = "loom"))]
    pub const fn new(t: T) -> Self {
        #[cfg(feature = "embassy")]
        {
            let inner = embassy_sync::blocking_mutex::Mutex::new(t);
            return Mutex { inner };
        }
        #[cfg(feature = "std")]
        {
            let inner = std::sync::Mutex::new(t);
            return Mutex { inner };
        }
    }

    #[cfg(feature = "loom")]
    pub fn new(t: T) -> Self {
        let inner = loom::sync::Mutex::new(t);
        Mutex { inner }
    }
}
