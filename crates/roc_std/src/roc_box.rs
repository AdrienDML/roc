#![deny(unsafe_op_in_unsafe_fn)]

use crate::{roc_alloc, roc_dealloc, storage::Storage};
use core::{
    cell::Cell,
    cmp::{self, Ordering},
    fmt::Debug,
    mem,
    ops::Deref,
    ptr::{self, NonNull},
};

#[repr(C)]
pub struct RocBox<T> {
    contents: NonNull<T>,
}

impl<T> RocBox<T> {
    pub fn new(contents: T) -> Self {
        let alignment = Self::alloc_alignment();
        let bytes = mem::size_of::<T>() + alignment;

        let ptr = unsafe { roc_alloc(bytes, alignment as u32) };

        if ptr.is_null() {
            todo!("Call roc_panic with the info that an allocation failed.");
        }

        // Initialize the reference count.
        let refcount_one = Storage::new_reference_counted();
        unsafe { ptr.cast::<Storage>().write(refcount_one) };

        let contents = unsafe {
            let contents_ptr = ptr.cast::<u8>().add(alignment).cast::<T>();

            *contents_ptr = contents;

            // We already verified that the original alloc pointer was non-null,
            // and this one is the alloc pointer with `alignment` bytes added to it,
            // so it should be non-null too.
            NonNull::new_unchecked(contents_ptr)
        };

        Self { contents }
    }

    #[inline(always)]
    fn alloc_alignment() -> usize {
        mem::align_of::<T>().max(mem::align_of::<Storage>())
    }

    pub fn into_inner(self) -> T {
        unsafe { ptr::read(self.contents.as_ptr() as *mut T) }
    }

    fn storage(&self) -> &Cell<Storage> {
        let alignment = Self::alloc_alignment();

        unsafe {
            &*self
                .contents
                .as_ptr()
                .cast::<u8>()
                .sub(alignment)
                .cast::<Cell<Storage>>()
        }
    }

    // This is unsafe because before doing a deep_copy we need to check that
    // self is read_only.
    unsafe fn deep_copy(&self) -> Self {
        let contents = unsafe { ptr::read(self.contents.as_ptr()) };
        Self::new(contents)
    }
}

impl<T> Deref for RocBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.contents.as_ref() }
    }
}

impl<T, U> PartialEq<RocBox<U>> for RocBox<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &RocBox<U>) -> bool {
        self.deref() == other.deref()
    }
}

impl<T> Eq for RocBox<T> where T: Eq {}

impl<T, U> PartialOrd<RocBox<U>> for RocBox<T>
where
    T: PartialOrd<U>,
{
    fn partial_cmp(&self, other: &RocBox<U>) -> Option<cmp::Ordering> {
        let self_contents = unsafe { self.contents.as_ref() };
        let other_contents = unsafe { other.contents.as_ref() };

        self_contents.partial_cmp(other_contents)
    }
}

impl<T> Ord for RocBox<T>
where
    T: Ord,
{
    fn cmp(&self, other: &Self) -> Ordering {
        let self_contents = unsafe { self.contents.as_ref() };
        let other_contents = unsafe { other.contents.as_ref() };

        self_contents.cmp(other_contents)
    }
}

impl<T> Debug for RocBox<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.deref().fmt(f)
    }
}

impl<T> Clone for RocBox<T> {
    fn clone(&self) -> Self {
        let storage = self.storage();
        let mut new_storage = storage.get();

        // Increment the reference count
        if !new_storage.is_readonly() {
            new_storage.increment_reference_count();
            storage.set(new_storage);
        }

        Self {
            contents: self.contents,
        }
    }
}

impl<T> Drop for RocBox<T> {
    fn drop(&mut self) {
        let storage = self.storage();
        let contents = self.contents;

        // Decrease the list's reference count.
        let mut new_storage = storage.get();
        let needs_dealloc = new_storage.decrease();

        if needs_dealloc {
            unsafe {
                // Drop the stored contents.
                let contents_ptr = contents.as_ptr();

                mem::drop::<T>(ptr::read(contents_ptr));

                let alignment = Self::alloc_alignment();

                // Release the memory.
                roc_dealloc(
                    contents.as_ptr().cast::<u8>().sub(alignment).cast(),
                    alignment as u32,
                );
            }
        } else if !new_storage.is_readonly() {
            // Write the storage back.
            storage.set(new_storage);
        }
    }
}

// This is a RocBox that is checked to ensure it is unique or readonly such that it can be sent between threads safely.
#[repr(transparent)]
pub struct SendSafeRocBox<T>(RocBox<T>);

unsafe impl<T> Send for SendSafeRocBox<T> {}

impl<T> Clone for SendSafeRocBox<T> {
    fn clone(&self) -> Self {
        // Determine if self is read only.
        let is_readonly = {
            let storage_ = self.0.storage().get();
            let val = storage_.is_readonly();
            self.0.storage().set(storage_);
            val
        };

        if is_readonly {
            // In this case we can just take ownership
            // of the data as it is safe to be sent between threads.
            SendSafeRocBox(self.0.clone())
        } else {
            // This is not read only, do a deep copy.
            unsafe { SendSafeRocBox(self.0.deep_copy()) }
        }
    }
}

impl<T> From<RocBox<T>> for SendSafeRocBox<T> {
    fn from(b: RocBox<T>) -> Self {
        // Determine if the give is read only or
        // if its reference count is 1.
        let is_safe = {
            let storage_ = b.storage().get();
            let val = storage_.is_readonly() || storage_.is_unique();
            b.storage().set(storage_);
            val
        };

        if is_safe {
            // In this case we can just take ownership
            // of the data as it is safe to be sent between threads.
            SendSafeRocBox(b)
        } else {
            // This is not read only nor unique, do a deep copy.
            unsafe { SendSafeRocBox(b.deep_copy()) }
        }
    }
}

impl<T> From<SendSafeRocBox<T>> for RocBox<T> {
    fn from(ssb: SendSafeRocBox<T>) -> Self {
        ssb.0
    }
}
