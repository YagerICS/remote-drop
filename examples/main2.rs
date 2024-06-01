#![feature(allocator_api)]
use std::{alloc::Global, cell::RefCell};

use embassy_sync::blocking_mutex::Mutex;
use remote_drop::remote_drop::{Queue, RBox, ToDeallocate};

static DEALLOC_QUEUE: Queue<Global> = Mutex::new(RefCell::new(ToDeallocate::new()));

fn main() {
    let q: &'static Queue<Global> = &DEALLOC_QUEUE;
    let mut boxes = 0u8;
    loop {
        {
            for _ in 0..boxes {
                let _bx = RBox::try_new([0u8; 1000], Global, &DEALLOC_QUEUE);
            }
            boxes = boxes.wrapping_add(1);
        }
        q.lock(|q| {
            let q = &mut q.borrow_mut();
            while q.garbage_collect_one() {}
        });
    }
}
