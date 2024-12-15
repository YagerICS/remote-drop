# `remote-drop`

Provides a way to heap-allocate values which are not immediately deallocated upon `Drop`ping them, but 
are instead added to a queue of objects to be dropped later e.g. by a dedicated cleanup thread.

Useful for performance-sensitive multithreaded applications. You can allocate and/or deallocate
in a low-priority thread, while using the allocated values in a high-priority thread.

Does not require a global allocator or `std`. You can use any allocator via the standard allocator API.

Extremely low overhead - no extra allocations and only one extra memory word per allocated object.
Other approaches tend to A) require extra allocations and B) require the use of a global allocator.

Can use `embassy_sync` (for embedded), `std` (for non-embedded), or `loom` (for testing) mutexes for internal locking, selected by cargo feature.

Compared to `defer_drop`, which provides a similar capability:
* This library is `no_std`
* This library works with custom allocators (not just `Global`)
* This solution requires no extra allocations (`defer_drop` requires a second allocation when the reference is dropped)
* This solution performs zero heap operations in the high-priority thread
* The only locking required in the high-priority thread is to briefly lock the cleanup queue, for a single intrusive linked-list insert
