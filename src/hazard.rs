use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::domain::HazardDomain;
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};

const SLOT_FREE: usize = 0;
const SLOT_ACQUIRED: usize = 1;

pub struct HazardSlot {
    pub(crate) protected: AtomicUsize,
    pub(crate) next: AtomicPtr<HazardSlot>,
    active: AtomicUsize,
}

impl HazardSlot {
    pub fn new() -> Self {
        Self {
            protected: AtomicUsize::new(0),
            next: AtomicPtr::new(std::ptr::null_mut()),
            active: AtomicUsize::new(SLOT_FREE),
        }
    }

    pub fn new_acquired() -> Self {
        Self {
            protected: AtomicUsize::new(0),
            next: AtomicPtr::new(std::ptr::null_mut()),
            active: AtomicUsize::new(SLOT_ACQUIRED),
        }
    }

    pub fn try_acquire(&self) -> bool {
        self.active
            .compare_exchange(SLOT_FREE, SLOT_ACQUIRED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn release(&self) {
        self.protected.store(0, Ordering::Release);
        self.active.store(SLOT_FREE, Ordering::Release);
    }
}

impl Default for HazardSlot {
    fn default() -> Self {
        Self::new()
    }
}

pub struct HazardPointer<'domain> {
    domain: &'domain HazardDomain,
    slot: NonNull<HazardSlot>,
}

impl<'domain> HazardPointer<'domain> {
    pub fn new(domain: &'domain HazardDomain) -> Self {
        let slot = domain.acquire();
        Self { domain, slot }
    }

    pub fn protect<T>(&self, src: &AtomicPtr<T>) -> Option<HazardGuard<'_, T>> {
        loop {
            let ptr = src.load(Ordering::Relaxed);
            if ptr.is_null() {
                self.clear();
                return None;
            }

            unsafe {
                self.slot
                    .as_ref()
                    .protected
                    .store(ptr as usize, Ordering::SeqCst);
            }

            if src.load(Ordering::SeqCst) == ptr {
                return Some(HazardGuard {
                    ptr,
                    hazard: self,
                    _marker: PhantomData,
                });
            }
        }
    }

    pub fn protect_ptr<T>(&self, ptr: *mut T) -> Option<HazardGuard<'_, T>> {
        if ptr.is_null() {
            self.clear();
            return None;
        }

        unsafe {
            self.slot
                .as_ref()
                .protected
                .store(ptr as usize, Ordering::SeqCst);
        }

        Some(HazardGuard {
            ptr,
            hazard: self,
            _marker: PhantomData,
        })
    }

    pub fn clear(&self) {
        unsafe {
            self.slot.as_ref().protected.store(0, Ordering::SeqCst);
        }
    }

    pub fn domain(&self) -> &'domain HazardDomain {
        self.domain
    }
}

impl Drop for HazardPointer<'_> {
    fn drop(&mut self) {
        self.domain.release(self.slot);
    }
}

unsafe impl Send for HazardPointer<'_> {}

pub struct HazardGuard<'hp, T> {
    ptr: *mut T,
    hazard: &'hp HazardPointer<'hp>,
    _marker: PhantomData<&'hp T>,
}

impl<T> HazardGuard<'_, T> {
    pub fn as_ref(&self) -> &T {
        unsafe { &*self.ptr }
    }

    pub fn as_ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<T> std::ops::Deref for HazardGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> Drop for HazardGuard<'_, T> {
    fn drop(&mut self) {
        self.hazard.clear();
    }
}

unsafe impl<T: Send> Send for HazardGuard<'_, T> {}
unsafe impl<T: Sync> Sync for HazardGuard<'_, T> {}

