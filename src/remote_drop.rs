use core::alloc::Allocator;
use core::cell::RefCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::sync::atomic::{self, AtomicUsize};
use core::{alloc::AllocError, mem::ManuallyDrop};

use alloc::alloc::Global;
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::mutex::Mutex;
extern crate alloc;

/**
Contains the items that have been pseudo-dropped in other threads and need
to be actually dropped in the GC thread.
*/
pub struct Queue<A: Allocator> {
    head: Mutex<core::cell::RefCell<ToDeallocate<A>>>,
}

/**
We artificially loosened the `Send` semantics for
QueueStorageBox, so we have to tighten them back up for the queue as a whole.
We shouldn't be able to send the queue between threads unless we can also send the allocator,
since we can take Box<_,A>s out of the queue.
    */
unsafe impl<A: Allocator + Send> Send for Queue<A> {}

impl<A: Allocator + 'static> Queue<A> {
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Queue<A> {
        let head = Mutex::new(RefCell::new(ToDeallocate::new()));
        Queue { head }
    }

    #[cfg(feature = "loom")]
    pub fn new() -> Queue<A> {
        let head = Mutex::new(RefCell::new(ToDeallocate::new()));
        Queue { head }
    }

    pub fn garbage_collect_one(&self) -> bool {
        let garbage: Option<Box<dyn DerefNode<A>, A>> =
            self.head.lock(|q| q.borrow_mut().extract_one());
        garbage.is_some()
    }

    pub fn garbage_collect_several<const N: usize>(&self) -> usize {
        let garbage: heapless::Vec<_, N> = self.head.lock(|q| q.borrow_mut().extract_several());
        garbage.len()
    }
}

/*
Holds a heap-allocated object.
The object will not be immediately `drop`ped on the `RBox` being dropped.
Instead, the object will be appended to a queue of objects to be dropped later.
This operation is very fast and predictable.
*/
pub struct RBox<T: Send + 'static, A: Allocator + 'static = Global> {
    // We use ManuallyDrop so we can take ownership from the &mut
    // provided to Drop::drop()
    item: ManuallyDrop<alloc::boxed::Box<DN<T, A>, A>>,
    queue: &'static Queue<A>,
}

/*
We store this in an RBox and then pull out the raw pointer for use in an Arc
*/
type ArcInner<T, A> = DN<(T, AtomicUsize), A>;

/*
Shared heap-allocated objects.
Immutable references only.
Last person to drop the RArc will drop the underlying RBox.
Can be cloned, if `A` can be.
Putting `Clone` requirement here so we can use it in `Drop`,
which makes writing `Drop` easier.
*/
pub struct RArc<
    T: Send + /* Not totally sure if Sync is required here for semantic sanity */ Sync + 'static,
    A: Allocator + Clone + 'static = Global,
> {
    // We use a NonNull instead of *mut to
    // achieve covariance and indicate that this is never a null pointer
    item: NonNull<ArcInner<T, A>>, // This is the underlying `Box`'s internal pointer
    queue: &'static Queue<A>,
    allocator: A,
    // Tell the drop checker that we (may) drop this type
    phantom: PhantomData<ArcInner<T, A>>,
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
unsafe impl<T: Send + Sync, A: Allocator + 'static> Sync for RBox<T, A> {}

unsafe impl<T: Send + Sync, A: Allocator + Clone + 'static> Send for RArc<T, A> {}
unsafe impl<T: Send + Sync, A: Allocator + Clone + 'static> Sync for RArc<T, A> {}

impl<T: Send + 'static, A: Allocator + 'static> Deref for RBox<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item.deref().deref().t
    }
}

impl<T: Send + Sync + 'static, A: Allocator + Clone + 'static> Deref for RArc<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &self.item.as_ref().t.0 }
    }
}

impl<T: Send + 'static, A: Allocator + 'static> DerefMut for RBox<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.item.deref_mut().deref_mut().t
    }
}

impl<T: Send + Sync + 'static, A: Allocator + Clone + 'static> Clone for RArc<T, A> {
    fn clone(&self) -> Self {
        let item = unsafe { self.item.as_ref() };
        let borrows = item.t.1.fetch_add(1, Relaxed);
        // Copying stdlib here. I don't think necessary to cap at 50% but whatever
        if borrows >= isize::MAX as usize {
            panic!("Max borrows reached");
        }
        RArc {
            queue: self.queue,
            item: self.item,
            allocator: self.allocator.clone(),
            phantom: PhantomData,
        }
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

impl<T: Send + Sync + 'static, A: Allocator + Clone + 'static> Drop for RArc<T, A> {
    fn drop(&mut self) {
        let inner = unsafe { self.item.as_ref() };
        // We want the thread that ultimately deletes this arc
        // (which may not be us) to see all of our writes, so we do this release.
        let remaining = inner.t.1.fetch_sub(1, Release);

        if remaining == 1 {
            // We are the last person with a ref to this item, so we need to drop it

            // We need to make sure see all the writes from other threads before dropping.
            // Copied from stdlib Arc.
            // Not totally clear to me why we use a fence instead of synchronizing on `inner.t.1`.
            // Maybe to make the happy path where we don't drop more performant?
            atomic::fence(Acquire);

            // Reconstruct the rbox
            let rbox = RBox {
                item: ManuallyDrop::new(unsafe {
                    Box::from_raw_in(
                        self.item.as_ptr(),
                        // NB: We could just put the Allocator in a ManuallyDrop and steal it here,
                        // but I'm just taking advantage of us needing it to be Clone anyway for core Arc functionality
                        self.allocator.clone(),
                    )
                }),
                queue: self.queue,
            };
            // Puts this back on the GC queue
            core::mem::drop(rbox);
        }
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

    fn into_inner(mut self: Self) -> (Box<DN<T, A>, A>, &'static Queue<A>) {
        let item = unsafe { ManuallyDrop::take(&mut self.item) };
        let queue = self.queue;
        core::mem::forget(self);
        (item, queue)
    }
}

impl<T: Send + Sync, A: Allocator + Clone> RArc<T, A> {
    pub fn try_new(item: T, alloc: A, queue: &'static Queue<A>) -> Result<Self, AllocError> {
        let (item, queue) =
            RBox::into_inner(RBox::try_new((item, AtomicUsize::new(1)), alloc, queue)?);
        let (ptr, alloc) = Box::into_raw_with_allocator(item);
        Ok(RArc {
            item: NonNull::new(ptr).expect("Null pointer in RArc::try_new"),
            allocator: alloc,
            queue,
            phantom: PhantomData,
        })
    }
}

// A vector that will not be mutated or dropped until unfrozen,
// therefore incurring no allocator operations
pub struct FrozenVec<T: Send, A: Allocator> {
    vec: Vec<T, A>,
}

// Even if Allocator : !Send, this is Send, because
// the vector will only be unfrozen and operated on in the GC thread,
// which is guaranteed to have safe access to the Allocator via type
// constraints on Queue<A>.
unsafe impl<T: Send, A: Allocator> Send for FrozenVec<T, A> {}

/**
A slice of values that can be dropped without invoking the underlying
`Drop::drop` or any allocator operations in the dropping thread.
*/
pub struct RSlice<T: Send + 'static, A: Allocator + 'static = Global> {
    slice: RBox<FrozenVec<T, A>, A>,
}

impl<T: Send + 'static, A: Allocator + 'static> RSlice<T, A> {
    pub fn try_new(
        items: Vec<T, A>,
        alloc: A,
        queue: &'static Queue<A>,
    ) -> Result<Self, AllocError> {
        Ok(RSlice {
            slice: RBox::try_new(FrozenVec { vec: items }, alloc, queue)?,
        })
    }
}

impl<T: Send + 'static, A: Allocator + 'static> Deref for RSlice<T, A> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.slice.deref().vec.deref()
    }
}

impl<T: Send + 'static, A: Allocator + 'static> DerefMut for RSlice<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slice.deref_mut().vec.deref_mut()
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

    fn extract_one(&mut self) -> Option<Box<dyn DerefNode<A>, A>> {
        let node = core::mem::replace(&mut self.next, None);
        if let Some(mut node) = node {
            self.next = node.the_box.get_next();
            Some(ManuallyDrop::into_inner(node.the_box))
        } else {
            None
        }
    }

    fn extract_several<const N: usize>(&mut self) -> heapless::Vec<Box<dyn DerefNode<A>, A>, N> {
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

    use std::alloc::Global;
    use std::vec::Vec;

    #[cfg(not(feature = "loom"))]
    use std::{sync::mpsc, sync::Arc, sync::Mutex, thread};

    #[cfg(feature = "loom")]
    use loom::{sync::mpsc, sync::Arc, sync::Mutex, thread};

    use std::collections::BTreeMap;
    use talc::{ErrOnOom, Talc, Talck};

    use crate::remote_drop::{Queue, RArc, RBox};

    // Central place for tracking deallocation order
    struct DeallocTracker<Id> {
        dealloc_map: BTreeMap<usize, Id>,
    }

    impl<Id> DeallocTracker<Id> {
        pub fn register(&mut self, id: Id) {
            self.dealloc_map.insert(self.dealloc_map.len(), id);
        }
    }

    // A clonable object we can pass around to track when
    // things get dropped
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

    // An object that registers when it gets dropped.
    // Stores its ID in the `DT` upon drop().
    struct D(u64, DT<u64>);

    impl Drop for D {
        fn drop(&mut self) {
            println!("Dropping {}", self.0);
            self.1 .0.lock().unwrap().register(self.0);
        }
    }

    fn run<F: Send + Sync + 'static + Fn()>(f: F) {
        #[cfg(feature = "loom")]
        loom::model(f);
        #[cfg(not(feature = "loom"))]
        f()
    }

    // Simple case - global allocator.
    #[test]
    fn test_global_drop() {
        run(|| {
            let tracker = DT::new();
            let mut expected = BTreeMap::new();
            let q: &'static Queue<Global> = Box::leak(Box::new(Queue::new()));
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
            // Clean up all the dropped values.
            while q.garbage_collect_one() {}
            assert_eq!(expected, tracker.results())
        });
    }

    // Slightly more complex - using a custom allocator.
    #[test]
    fn test_talc_drop() {
        run(|| {
            static TALCK: Talck<spin::Mutex<()>, ErrOnOom> =
                Talc::new(ErrOnOom).lock::<spin::Mutex<()>>();

            let arena = Vec::leak([0u8].into_iter().cycle().take(8 * 1024).collect());
            unsafe {
                TALCK.lock().claim(arena.into()).unwrap();
            }

            type Alloc = &'static Talck<spin::Mutex<()>, ErrOnOom>;
            let q: &'static Queue<Alloc> = Box::leak(Box::new(Queue::new()));
            let tracker = DT::new();
            let mut expected = BTreeMap::new();
            let a: Alloc = &TALCK;
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
        });
    }

    // Using a custom allocator *and* sending values to other threads.
    // Despite being dropped in other threads, the underlying drop()
    // function won't be called until we garbage collect them.
    #[test]
    fn test_talc_send() {
        run(|| {
            let arena = Vec::leak([0u8].into_iter().cycle().take(8 * 1024).collect());
            static TALCK: Talck<spin::Mutex<()>, ErrOnOom> =
                Talc::new(ErrOnOom).lock::<spin::Mutex<()>>();
            unsafe {
                TALCK.lock().claim(arena.into()).unwrap();
            }

            type Alloc = &'static Talck<spin::Mutex<()>, ErrOnOom>;
            let q: &'static Queue<Alloc> = Box::leak(Box::new(Queue::new()));
            let tracker = DT::new();
            let mut expected = BTreeMap::new();
            let a: Alloc = &TALCK;

            // Create a couple RBox values and send them off to other threads.
            let box1 = RBox::try_new(D(1, tracker.clone()), a, q).unwrap();
            let (s1, r1) = mpsc::channel();
            let t1 = thread::spawn(move || {
                r1.recv().unwrap();
                std::mem::drop(box1);
            });

            let box2 = RBox::try_new(D(2, tracker.clone()), a, q).unwrap();
            let (s2, r2) = mpsc::channel();
            let t2 = thread::spawn(move || {
                r2.recv().unwrap();
                std::mem::drop(box2);
            });

            // Create an RBox value that gets dropped in this thread.
            {
                let _n3 = D(3, tracker.clone());
            }

            expected.insert(0, 3);
            while q.garbage_collect_one() {}
            assert_eq!(expected, tracker.results());

            s1.send(()).unwrap();
            t1.join().unwrap();
            // The RBox has been dropped from thread 1, but the stored value
            // has not been GC'd yet!
            assert_eq!(expected, tracker.results());
            while q.garbage_collect_one() {}
            // Now the stored value has been GC'd
            expected.insert(1, 1);
            assert_eq!(expected, tracker.results());

            s2.send(()).unwrap();
            t2.join().unwrap();
            assert_eq!(expected, tracker.results());
            while q.garbage_collect_one() {}
            expected.insert(2, 2);
            assert_eq!(expected, tracker.results());
        });
    }

    #[test]
    fn test_talc_send_arc() {
        run(|| {
            let arena = Vec::leak([0u8].into_iter().cycle().take(8 * 1024).collect());
            static TALCK: Talck<spin::Mutex<()>, ErrOnOom> =
                Talc::new(ErrOnOom).lock::<spin::Mutex<()>>();
            unsafe {
                TALCK.lock().claim(arena.into()).unwrap();
            }

            type Alloc = &'static Talck<spin::Mutex<()>, ErrOnOom>;
            let q: &'static Queue<Alloc> = Box::leak(Box::new(Queue::new()));
            let tracker = DT::new();
            let mut expected = BTreeMap::new();
            let a: Alloc = &TALCK;

            let arc1 = RArc::try_new(D(1, tracker.clone()), a, q).unwrap();
            let arc1_a = arc1.clone();
            let (s1, r1) = mpsc::channel();
            let (s2, r2) = mpsc::channel();
            let t1 = thread::spawn(move || {
                r1.recv().unwrap();
                std::mem::drop(arc1_a);
                s2.send(()).unwrap();
            });

            while q.garbage_collect_one() {}
            assert_eq!(expected, tracker.results());

            s1.send(()).unwrap();
            r2.recv().unwrap();

            while q.garbage_collect_one() {}
            assert_eq!(expected, tracker.results());

            expected.insert(0, 1);
            std::println!("arc1 drop");
            std::mem::drop(arc1);

            while q.garbage_collect_one() {}
            assert_eq!(expected, tracker.results());
        });
    }
}
