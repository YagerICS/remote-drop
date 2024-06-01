use core::alloc::Allocator;
use core::ops::{Deref, DerefMut};
use core::{alloc::AllocError, mem::ManuallyDrop};

use alloc::alloc::Global;
use alloc::boxed::Box;
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
extern crate alloc;

pub type Queue<A> = Mutex<CriticalSectionRawMutex, core::cell::RefCell<ToDeallocate<A>>>;

/*
Holds a heap-allocated object.
The object will not be immediately `drop`ped on the `RBox` being dropped.
Instead, the object will be appended to a queue of objects to be dropped later.
This operation is very fast and predictable.
*/
pub struct RBox<T: Send + 'static, A: Allocator + 'static + Sync + Send = Global> {
    item: ManuallyDrop<alloc::boxed::Box<DN<T, A>, A>>,
    queue: &'static Queue<A>,
}

impl<T: Send + 'static, A: Allocator + 'static + Sync + Send> Deref for RBox<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item.deref().deref().t
    }
}

impl<T: Send + 'static, A: Allocator + 'static + Sync + Send> DerefMut for RBox<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.item.deref_mut().deref_mut().t
    }
}

impl<T: Send + 'static, A: Allocator + 'static + Sync + Send> Drop for RBox<T, A> {
    fn drop(&mut self) {
        let item = unsafe { ManuallyDrop::take(&mut self.item) };
        self.queue.lock(|q| q.borrow_mut().add_item(item));
    }
}

impl<T: Send, A: Allocator + Sync + Send> RBox<T, A> {
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

pub trait DerefNode<A: Allocator>: Send {
    // Can't be Self because maybe unsized
    fn get_next(&mut self) -> Option<Box<dyn DerefNode<A>, A>>;
}

/*
An intrusive linked-list node which can be appended to the cleanup queue.
*/
struct DN<T, A: Allocator> {
    t: T,
    next: Option<Box<dyn DerefNode<A>, A>>,
}

impl<T: Send, A: Allocator + Send> DerefNode<A> for DN<T, A> {
    fn get_next(&mut self) -> Option<Box<dyn DerefNode<A>, A>> {
        core::mem::replace(&mut self.next, None)
    }
}

impl<A: Allocator + Send, T: DerefNode<A>> DerefNode<A> for Box<DN<T, A>, A> {
    fn get_next(&mut self) -> Option<Box<dyn DerefNode<A>, A>> {
        core::mem::replace(&mut self.next, None)
    }
}

pub struct ToDeallocate<A: Allocator + Sync> {
    next: Option<Box<dyn DerefNode<A>, A>>,
}

impl<A: Allocator + Sync> ToDeallocate<A> {
    pub const fn new() -> Self {
        ToDeallocate { next: None }
    }
}

impl<A: Allocator + 'static + Send + Sync> ToDeallocate<A> {
    fn add_item<T: Send + 'static>(&mut self, mut node: Box<DN<T, A>, A>) {
        node.next = core::mem::replace(&mut self.next, None);
        self.next = Some(node as Box<dyn DerefNode<A>, A>);
    }

    pub fn extract_one(&mut self) -> Option<Box<dyn DerefNode<A>, A>> {
        let node = core::mem::replace(&mut self.next, None);
        if let Some(mut node) = node {
            self.next = node.get_next();
            Some(node)
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
