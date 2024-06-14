use core::alloc::Allocator;
use core::borrow::BorrowMut;
use core::cell::RefCell;
use core::ops::{Deref, DerefMut};
use core::{alloc::AllocError, mem::ManuallyDrop};

use alloc::alloc::Global;
use alloc::boxed::Box;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
extern crate alloc;

/**
Contains the items that have been pseudo-dropped in other threads and need
to be actuall dropped in the GC thread.
*/
pub struct Queue<A: Allocator> {
    head: Mutex<CriticalSectionRawMutex, core::cell::RefCell<ToDeallocate<A>>>,
}

impl<A: Allocator + 'static> Queue<A> {
    pub const fn new() -> Queue<A> {
        let head = Mutex::new(RefCell::new(ToDeallocate::new()));
        Queue { head }
    }

    pub fn garbage_collect_one(&self) -> bool {
        let garbage: Option<Box<dyn DerefNode<A>, A>> =
            self.head.lock(|q| q.borrow_mut().extract_one());
        garbage.is_some()
    }

    pub fn garbage_collect_several<const N: usize>(&self) -> bool {
        let garbage: heapless::Vec<_, N> = self.head.lock(|q| q.borrow_mut().extract_several());
        !garbage.is_empty()
    }
}

/*
Holds a heap-allocated object.
The object will not be immediately `drop`ped on the `RBox` being dropped.
Instead, the object will be appended to a queue of objects to be dropped later.
This operation is very fast and predictable.
*/
pub struct RBox<T: Send + 'static, A: Allocator + 'static = Global> {
    item: ManuallyDrop<alloc::boxed::Box<DN<T, A>, A>>,
    queue: &'static Queue<A>,
}

/**
We don't require that `A` is `Send`, because
we never do any operations on the `Box<_,A>` from an `RBox` object.
It's possible that `T` uses `A`, in which case that will indirectly
require `A` to be `Send`, but as far as the core function of this
library, we can freely send `RBox`es without worrying about whether
the underlying `Box` is `Send`.
    */
unsafe impl<T: Send, A: Allocator + 'static> Send for RBox<T, A> {}

impl<T: Send + 'static, A: Allocator + 'static> Deref for RBox<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item.deref().deref().t
    }
}

impl<T: Send + 'static, A: Allocator + 'static> DerefMut for RBox<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.item.deref_mut().deref_mut().t
    }
}

/**
Take the thing out of the `RBox` and put it on the deallocation queue.
*/
impl<T: Send + 'static, A: Allocator + 'static> Drop for RBox<T, A> {
    fn drop(&mut self) {
        // We remove it from `ManuallyDrop` here.
        // Safe to `take` because nothing else will have access to this item
        // after dropping.
        let item = unsafe { ManuallyDrop::take(&mut self.item) };
        // We'll put it back in a `ManuallyDrop` in here
        self.queue.head.lock(|q| q.borrow_mut().add_item(item));
    }
}

impl<T: Send, A: Allocator> RBox<T, A> {
    pub fn try_new(item: T, alloc: A, queue: &'static Queue<A>) -> Result<Self, AllocError> {
        Ok(RBox {
            item: ManuallyDrop::new(Box::try_new_in(
                DN {
                    t: item,
                    next: None,
                },
                alloc,
            )?),
            queue,
        })
    }
}

/**
We store things in here when they're sitting on the cleanup queue.
*/
struct QueueStorageBox<A: Allocator> {
    the_box: ManuallyDrop<Box<dyn DerefNode<A>, A>>,
}

/**
The underlying `T` that was erased before storage
is `Send`, and we don't create `RBox`es except on the thread where
the `A` is valid, so this is safe to send.
*/
unsafe impl<A: Allocator> Send for QueueStorageBox<A> {}

/*
An intrusive linked-list node which can be appended to the cleanup queue.
*/
struct DN<T, A: Allocator> {
    t: T,
    next: Option<QueueStorageBox<A>>,
}

/**
We degrade DN<T,A> into DerefNode<A> so we can hide
the type parameter T before storing on the cleanup queue.
*/
trait DerefNode<A: Allocator> {
    // Remove the next node (if present) from this linked list node.
    // Can't be Self because maybe unsized
    fn get_next(&mut self) -> Option<QueueStorageBox<A>>;
}

/**
Deallocs only occur wherever we're allowed to use `A`.
*/
unsafe impl<T: Send, A: Allocator> Send for DN<T, A> {}

/**
Before putting the DN on the cleanup queue, we
degrate it to one of these. This is all we really need from it.
*/
impl<T: Send, A: Allocator> DerefNode<A> for DN<T, A> {
    fn get_next(&mut self) -> Option<QueueStorageBox<A>> {
        core::mem::replace(&mut self.next, None)
    }
}

/**
The head of the cleanup linked list.
*/
pub struct ToDeallocate<A: Allocator> {
    next: Option<QueueStorageBox<A>>,
}

impl<A: Allocator> ToDeallocate<A> {
    pub const fn new() -> Self {
        ToDeallocate { next: None }
    }
}

impl<A: Allocator + 'static> ToDeallocate<A> {
    fn add_item<T: Send + 'static>(&mut self, mut node: Box<DN<T, A>, A>) {
        node.next = core::mem::replace(&mut self.next, None);
        self.next = Some(QueueStorageBox {
            the_box: ManuallyDrop::new(node as Box<dyn DerefNode<A>, A>),
        });
    }

    pub fn extract_one(&mut self) -> Option<Box<dyn DerefNode<A>, A>> {
        let node = core::mem::replace(&mut self.next, None);
        if let Some(mut node) = node {
            self.next = node.the_box.get_next();
            Some(ManuallyDrop::into_inner(node.the_box))
        } else {
            None
        }
    }

    pub fn extract_several<const N: usize>(
        &mut self,
    ) -> heapless::Vec<Box<dyn DerefNode<A>, A>, N> {
        let mut vec = heapless::Vec::new();
        while vec.len() < vec.capacity()
            && let Some(node) = self.extract_one()
        {
            vec.push(node)
                .map_err(|_| "BUG: Could not push node")
                .unwrap();
        }
        vec
    }
}

#[cfg(test)]
mod test {
    use std::{alloc::Global, collections::HashMap, sync::Mutex};

    use std::sync::Arc;

    use std::collections::BTreeMap;
    use talc::{ErrOnOom, Talc, Talck};

    use crate::remote_drop::{Queue, RBox};

    struct DeallocTracker<Id> {
        dealloc_map: BTreeMap<usize, Id>,
    }

    impl<Id> DeallocTracker<Id> {
        pub fn register(&mut self, id: Id) {
            self.dealloc_map.insert(self.dealloc_map.len(), id);
        }
    }

    #[derive(Clone)]
    struct DT<Id>(Arc<Mutex<DeallocTracker<Id>>>);

    impl<Id: Clone> DT<Id> {
        pub fn new() -> Self {
            DT(Arc::new(Mutex::new(DeallocTracker {
                dealloc_map: BTreeMap::new(),
            })))
        }
        pub fn results(&self) -> BTreeMap<usize, Id> {
            self.0.lock().unwrap().dealloc_map.clone()
        }
    }

    struct D(u64, DT<u64>);

    impl Drop for D {
        fn drop(&mut self) {
            println!("Dropping {}", self.0);
            self.1 .0.lock().unwrap().register(self.0);
        }
    }

    #[test]
    fn test_global_drop() {
        static DEALLOC_QUEUE: Queue<Global> = Queue::new();
        let tracker = DT::new();
        let mut expected = BTreeMap::new();
        let q: &'static Queue<Global> = &DEALLOC_QUEUE;
        {
            // "Dropped" second, first on cleanup queue
            let _box1 = RBox::try_new(D(1, tracker.clone()), Global, q).unwrap();
            expected.insert(1, 1);
            // "Dropped" first, second on cleanup queue
            let _box2 = RBox::try_new(D(2, tracker.clone()), Global, q).unwrap();
            expected.insert(2, 2);
            let _n3 = D(3, tracker.clone());
            // n3 should get deallocated first
            expected.insert(0, 3);
        }
        println!("Done allocating");
        while q.garbage_collect_one() {}
        assert_eq!(expected, tracker.results())
    }

    #[test]
    fn test_talc_drop() {
        static mut ARENA: [u8; 8 * 1024] = [0; 8 * 1024];
        static TALCK: Talck<spin::Mutex<()>, ErrOnOom> =
            Talc::new(ErrOnOom).lock::<spin::Mutex<()>>();
        unsafe {
            TALCK.lock().claim(ARENA.as_mut().into()).unwrap();
        }

        type Alc = &'static Talck<spin::Mutex<()>, ErrOnOom>;
        static DEALLOC_QUEUE: Queue<Alc> = Queue::new();
        let tracker = DT::new();
        let mut expected = BTreeMap::new();
        let q: &'static Queue<Alc> = &DEALLOC_QUEUE;
        let a: Alc = &TALCK;
        {
            // "Dropped" second, first on cleanup queue
            let _box1 = RBox::try_new(D(1, tracker.clone()), a, q).unwrap();
            expected.insert(1, 1);
            // "Dropped" first, second on cleanup queue
            let _box2 = RBox::try_new(D(2, tracker.clone()), a, q).unwrap();
            expected.insert(2, 2);
            let _n3 = D(3, tracker.clone());
            // n3 should get deallocated first
            expected.insert(0, 3);
        }
        println!("Done allocating");
        while q.garbage_collect_one() {}
        assert_eq!(expected, tracker.results())
    }
}
