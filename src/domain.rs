use std::collections::HashSet;
use std::ptr::NonNull;

use crate::hazard::HazardSlot;
use crate::retired::RetiredList;
use crate::sync::{AtomicPtr, Mutex, Ordering};

pub struct HazardDomain {
    hazards: AtomicPtr<HazardSlot>,
    retired: Mutex<RetiredList>,
    reclaim_threshold: usize,
}

impl HazardDomain {
    pub fn new() -> Self {
        Self::with_threshold(64)
    }

    pub fn with_threshold(reclaim_threshold: usize) -> Self {
        Self {
            hazards: AtomicPtr::new(std::ptr::null_mut()),
            retired: Mutex::new(RetiredList::new()),
            reclaim_threshold,
        }
    }

    pub fn acquire(&self) -> NonNull<HazardSlot> {
        let mut current = self.hazards.load(Ordering::Acquire);
        while !current.is_null() {
            let slot = unsafe { &*current };
            if slot.try_acquire() {
                return NonNull::new(current).unwrap();
            }
            current = slot.next.load(Ordering::Acquire);
        }

        let new_slot = Box::into_raw(Box::new(HazardSlot::new_acquired()));

        loop {
            let head = self.hazards.load(Ordering::Acquire);
            unsafe { (*new_slot).next.store(head, Ordering::Relaxed) };

            match self.hazards.compare_exchange_weak(
                head,
                new_slot,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return NonNull::new(new_slot).unwrap(),
                Err(_) => continue,
            }
        }
    }

    pub fn release(&self, slot: NonNull<HazardSlot>) {
        unsafe {
            slot.as_ref().release();
        }
    }

    pub fn retire<T>(&self, ptr: *mut T) {
        self.retire_with_deleter(ptr, |p| {
            let _ = unsafe { Box::from_raw(p as *mut T) };
        });
    }

    pub fn retire_with_deleter<T>(&self, ptr: *mut T, deleter: fn(*mut u8)) {
        let should_reclaim = {
            let mut retired = self.retired.lock().unwrap();
            retired.push(ptr as *mut u8, deleter);
            retired.len() >= self.reclaim_threshold
        };

        if should_reclaim {
            self.reclaim();
        }
    }

    pub fn reclaim(&self) {
        let mut retired = self.retired.lock().unwrap();
        let protected = self.collect_protected();
        retired.reclaim(&protected);
    }

    fn collect_protected(&self) -> HashSet<usize> {
        loop {
            let mut protected = HashSet::new();
            let head = self.hazards.load(Ordering::Acquire);
            let mut current = head;

            while !current.is_null() {
                let slot = unsafe { &*current };
                let addr = slot.protected.load(Ordering::SeqCst);
                if addr != 0 {
                    protected.insert(addr);
                }
                current = slot.next.load(Ordering::Acquire);
            }

            if self.hazards.load(Ordering::Acquire) == head {
                return protected;
            }
        }
    }

    pub fn retired_count(&self) -> usize {
        self.retired.lock().unwrap().len()
    }
}

impl Default for HazardDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HazardDomain {
    fn drop(&mut self) {
        let mut current = self.hazards.load(Ordering::Relaxed);
        while !current.is_null() {
            let slot = unsafe { Box::from_raw(current) };
            current = slot.next.load(Ordering::Relaxed);
        }

        let retired = self.retired.get_mut().unwrap();
        retired.force_reclaim_all();
    }
}

unsafe impl Send for HazardDomain {}
unsafe impl Sync for HazardDomain {}

