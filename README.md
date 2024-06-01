# `remote-drop`

Provides a way to heap-allocate values which are not immediately deallocated upon `Drop`ping them, but 
are instead added to a queue of objects to be dropped later e.g. by a dedicated cleanup thread.

Useful for performance-sensitive multithreaded applications.

Does not require a global allocator or `std`. You can use any allocator via the standard allocator API.

Extremely low overhead - no extra allocations and only one extra memory word per allocated object.
Other approaches tend to A) require extra allocations and B) require the use of a global allocator.

Currently depends on `embassy_sync` for `CriticalSectionRawMutex` (used to lock the cleanup queue),
but it should be pretty easy to make this library parametric over choice of mutex.
