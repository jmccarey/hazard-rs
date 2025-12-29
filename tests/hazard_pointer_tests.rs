#![cfg(not(feature = "loom"))]

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use hazard::{HazardDomain, HazardPointer};

struct DropTracker {
    value: usize,
    counter: Arc<AtomicUsize>,
}

impl DropTracker {
    fn new(value: usize, counter: Arc<AtomicUsize>) -> Self {
        Self { value, counter }
    }

    fn boxed(value: usize, counter: Arc<AtomicUsize>) -> *mut Self {
        Box::into_raw(Box::new(Self::new(value, counter)))
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn protect_returns_none_for_null_pointer() {
    let domain = HazardDomain::new();
    let atomic: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic);

    assert!(guard.is_none());
}

#[test]
fn protect_returns_some_for_valid_pointer() {
    let domain = HazardDomain::new();
    let ptr = Box::into_raw(Box::new(42u64));
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic);

    assert!(guard.is_some());
    assert_eq!(*guard.unwrap(), 42);

    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn protect_retry_loop_handles_concurrent_modifications() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(1, counter.clone());
    let atomic = Arc::new(AtomicPtr::new(initial));

    let barrier = Arc::new(Barrier::new(2));

    let writer_domain = domain.clone();
    let writer_atomic = atomic.clone();
    let writer_barrier = barrier.clone();
    let writer_counter = counter.clone();

    let writer = thread::spawn(move || {
        writer_barrier.wait();
        for i in 2..=100 {
            let new_ptr = DropTracker::boxed(i, writer_counter.clone());
            let old = writer_atomic.swap(new_ptr, Ordering::SeqCst);
            writer_domain.retire(old);
        }
    });

    let reader_domain = domain.clone();
    let reader_atomic = atomic.clone();
    let reader_barrier = barrier.clone();

    let reader = thread::spawn(move || {
        reader_barrier.wait();
        for _ in 0..100 {
            let hp = HazardPointer::new(&reader_domain);
            if let Some(guard) = hp.protect(&reader_atomic) {
                let val = guard.value;
                assert!(val >= 1 && val <= 100);
            }
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    let final_ptr = atomic.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();
}

#[test]
fn protect_ptr_directly_protects_loaded_pointer() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect_ptr(ptr);

    assert!(guard.is_some());
    assert_eq!(guard.as_ref().unwrap().value, 42);

    domain.retire(ptr);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn protect_ptr_returns_none_for_null() {
    let domain = HazardDomain::new();
    let hp = HazardPointer::new(&domain);

    let guard = hp.protect_ptr::<u64>(std::ptr::null_mut());
    assert!(guard.is_none());
}

#[test]
fn clear_removes_protection() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic).unwrap();

    domain.retire(ptr);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard);
    hp.clear();

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn domain_returns_correct_reference() {
    let domain = HazardDomain::new();
    let hp = HazardPointer::new(&domain);

    let domain_ref = hp.domain();

    let counter = Arc::new(AtomicUsize::new(0));
    let ptr = DropTracker::boxed(0, counter.clone());
    domain_ref.retire(ptr);
    domain_ref.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn guard_deref_provides_direct_access() {
    let domain = HazardDomain::new();
    let ptr = Box::into_raw(Box::new(String::from("hello")));
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic).unwrap();

    assert_eq!(guard.len(), 5);
    assert_eq!(&*guard, "hello");

    drop(guard);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn guard_as_ref_works() {
    let domain = HazardDomain::new();
    let ptr = Box::into_raw(Box::new(vec![1, 2, 3]));
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic).unwrap();

    let vec_ref = guard.as_ref();
    assert_eq!(vec_ref.len(), 3);
    assert_eq!(vec_ref[0], 1);

    drop(guard);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn guard_as_ptr_works() {
    let domain = HazardDomain::new();
    let ptr = Box::into_raw(Box::new(42u64));
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic).unwrap();

    assert_eq!(guard.as_ptr(), ptr);

    drop(guard);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn guard_drop_clears_hazard_pointer() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);

    {
        let _guard = hp.protect(&atomic).unwrap();
        domain.retire(ptr);
        domain.reclaim();
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn hazard_pointer_reuse_after_drop() {
    let domain = HazardDomain::new();

    {
        let _hp1 = HazardPointer::new(&domain);
        let _hp2 = HazardPointer::new(&domain);
    }

    let hp3 = HazardPointer::new(&domain);
    let hp4 = HazardPointer::new(&domain);

    let ptr = Box::into_raw(Box::new(42u64));
    let atomic = AtomicPtr::new(ptr);

    let guard3 = hp3.protect(&atomic);
    let guard4 = hp4.protect(&atomic);

    assert!(guard3.is_some());
    assert!(guard4.is_some());

    drop(guard3);
    drop(guard4);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn multiple_guards_from_multiple_hazard_pointers() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr1 = DropTracker::boxed(1, counter.clone());
    let ptr2 = DropTracker::boxed(2, counter.clone());
    let ptr3 = DropTracker::boxed(3, counter.clone());

    let atomic1 = AtomicPtr::new(ptr1);
    let atomic2 = AtomicPtr::new(ptr2);
    let atomic3 = AtomicPtr::new(ptr3);

    let hp1 = HazardPointer::new(&domain);
    let hp2 = HazardPointer::new(&domain);
    let hp3 = HazardPointer::new(&domain);

    let guard1 = hp1.protect(&atomic1).unwrap();
    let guard2 = hp2.protect(&atomic2).unwrap();
    let guard3 = hp3.protect(&atomic3).unwrap();

    assert_eq!(guard1.value, 1);
    assert_eq!(guard2.value, 2);
    assert_eq!(guard3.value, 3);

    domain.retire(ptr1);
    domain.retire(ptr2);
    domain.retire(ptr3);

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard1);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    drop(guard2);
    drop(guard3);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[test]
fn re_protection_overwrites_previous() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr1 = DropTracker::boxed(1, counter.clone());
    let ptr2 = DropTracker::boxed(2, counter.clone());

    let atomic1 = AtomicPtr::new(ptr1);
    let atomic2 = AtomicPtr::new(ptr2);

    let hp = HazardPointer::new(&domain);

    let guard1 = hp.protect(&atomic1).unwrap();
    assert_eq!(guard1.value, 1);
    drop(guard1);

    domain.retire(ptr1);

    let guard2 = hp.protect(&atomic2).unwrap();
    assert_eq!(guard2.value, 2);

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    domain.retire(ptr2);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    drop(guard2);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn hazard_pointer_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<HazardPointer<'_>>();
}

#[test]
fn hazard_pointer_across_threads() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = Arc::new(AtomicPtr::new(ptr));

    let domain_clone = domain.clone();
    let atomic_clone = atomic.clone();
    let handle = thread::spawn(move || {
        let hp = HazardPointer::new(&domain_clone);
        let guard = hp.protect(&atomic_clone).unwrap();
        assert_eq!(guard.value, 42);
    });

    handle.join().unwrap();
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn protect_same_pointer_multiple_times() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let hp1 = HazardPointer::new(&domain);
    let hp2 = HazardPointer::new(&domain);

    let guard1 = hp1.protect(&atomic).unwrap();
    let guard2 = hp2.protect(&atomic).unwrap();

    assert_eq!(guard1.value, 42);
    assert_eq!(guard2.value, 42);
    assert_eq!(guard1.as_ptr(), guard2.as_ptr());

    domain.retire(ptr);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard1);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard2);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

