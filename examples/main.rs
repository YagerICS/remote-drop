#![feature(allocator_api)]
use std::{alloc::Global, cell::RefCell};

use embassy_sync::blocking_mutex::Mutex;
use remote_drop::remote_drop::{Queue, RBox, ToDeallocate};

static DEALLOC_QUEUE: Queue<Global> = Queue::new();

struct Noisy(u64);

impl Drop for Noisy {
    fn drop(&mut self) {
        println!("Dropping {}", self.0)
    }
}

fn main() {
    let q: &'static Queue<Global> = &DEALLOC_QUEUE;
    {
        // Will get added to the cleanup queue on drop
        let _box1 = RBox::try_new(Noisy(1), Global, q);
        let _box2 = RBox::try_new(Noisy(2), Global, q);
        // Will get dropped right away
        let _n3 = Noisy(3);
    }
    println!("Done allocating");
    // Clean up the stuff on the GC queue
    while q.garbage_collect_one() {}
}
