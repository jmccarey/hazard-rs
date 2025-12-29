#![cfg(not(feature = "loom"))]

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use hazard::{global_domain, protect, protect_ptr, reclaim, retire, retire_with_deleter};

struct DropTracker {
    value: usize,
    counter: Arc<AtomicUsize>,
}

impl DropTracker {
    fn boxed(value: usize, counter: Arc<AtomicUsize>) -> *mut Self {
        Box::into_raw(Box::new(Self { value, counter }))
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn global_domain_returns_consistent_singleton() {
    let domain1 = global_domain();
    let domain2 = global_domain();

    let ptr1 = domain1 as *const _;
    let ptr2 = domain2 as *const _;

    assert_eq!(ptr1, ptr2);
}

#[test]
fn protect_with_thread_local_hazard_pointer() {
    let counter = Arc::new(AtomicUsize::new(0));
    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let guard = protect(&atomic);
    assert!(guard.is_some());
    assert_eq!(guard.as_ref().unwrap().value, 42);

    retire(ptr);
    reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard);
    reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn protect_returns_none_for_null() {
    let atomic: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());
    let guard = protect(&atomic);
    assert!(guard.is_none());
}

#[test]
fn protect_ptr_with_thread_local_hazard_pointer() {
    let counter = Arc::new(AtomicUsize::new(0));
    let ptr = DropTracker::boxed(99, counter.clone());

    let guard = protect_ptr(ptr);
    assert!(guard.is_some());
    assert_eq!(guard.as_ref().unwrap().value, 99);

    retire(ptr);
    reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 0);

    drop(guard);
    reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn protect_ptr_returns_none_for_null() {
    let guard = protect_ptr::<u64>(std::ptr::null_mut());
    assert!(guard.is_none());
}

#[test]
fn retire_adds_to_global_domain() {
    let initial_count = global_domain().retired_count();

    let ptr = Box::into_raw(Box::new(42u64));
    retire(ptr);

    assert!(global_domain().retired_count() > initial_count);

    reclaim();
}

#[test]
fn retire_with_deleter_uses_custom_deleter() {
    static CUSTOM_DELETE_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn custom_deleter(ptr: *mut u8) {
        CUSTOM_DELETE_COUNT.fetch_add(1, Ordering::SeqCst);
        let _ = unsafe { Box::from_raw(ptr as *mut u64) };
    }

    let before = CUSTOM_DELETE_COUNT.load(Ordering::SeqCst);

    let ptr = Box::into_raw(Box::new(123u64));
    retire_with_deleter(ptr, custom_deleter);
    reclaim();

    let after = CUSTOM_DELETE_COUNT.load(Ordering::SeqCst);
    assert!(after > before);
}

#[test]
fn reclaim_triggers_global_domain_reclamation() {
    let counter = Arc::new(AtomicUsize::new(0));

    for i in 0..10 {
        let ptr = DropTracker::boxed(i, counter.clone());
        retire(ptr);
    }

    reclaim();

    assert!(counter.load(Ordering::SeqCst) > 0);
}

#[test]
fn thread_local_hp_persists_across_calls() {
    let ptr1 = Box::into_raw(Box::new(1u64));
    let ptr2 = Box::into_raw(Box::new(2u64));

    let atomic1 = AtomicPtr::new(ptr1);
    let atomic2 = AtomicPtr::new(ptr2);

    {
        let guard1 = protect(&atomic1);
        assert!(guard1.is_some());
        assert_eq!(*guard1.unwrap(), 1);
    }

    {
        let guard2 = protect(&atomic2);
        assert!(guard2.is_some());
        assert_eq!(*guard2.unwrap(), 2);
    }

    let _ = unsafe { Box::from_raw(ptr1) };
    let _ = unsafe { Box::from_raw(ptr2) };
}

#[test]
fn each_thread_gets_own_hazard_pointer() {
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(4));

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let counter = counter.clone();
            let barrier = barrier.clone();

            thread::spawn(move || {
                let ptr = DropTracker::boxed(i, counter.clone());
                let atomic = AtomicPtr::new(ptr);

                let guard = protect(&atomic);
                assert!(guard.is_some());

                barrier.wait();

                assert_eq!(guard.as_ref().unwrap().value, i);

                drop(guard);

                retire(ptr);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 4);
}

#[test]
fn concurrent_protect_and_retire() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(AtomicPtr::new(DropTracker::boxed(0, counter.clone())));
    let barrier = Arc::new(Barrier::new(3));

    let mut handles = vec![];

    for _ in 0..2 {
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..100 {
                if let Some(guard) = protect(&shared) {
                    let _ = guard.value;
                }
            }
        }));
    }

    {
        let counter = counter.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for i in 1..=50 {
                let new_ptr = DropTracker::boxed(i, counter.clone());
                let old = shared.swap(new_ptr, Ordering::SeqCst);
                retire(old);

                if i % 10 == 0 {
                    reclaim();
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        retire(final_ptr);
    }
    reclaim();
}

#[test]
fn multiple_protections_same_thread() {
    let ptr = Box::into_raw(Box::new(42u64));
    let atomic = AtomicPtr::new(ptr);

    let guard1 = protect(&atomic);
    assert!(guard1.is_some());

    drop(guard1);

    let guard2 = protect(&atomic);
    assert!(guard2.is_some());
    assert_eq!(*guard2.unwrap(), 42);

    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn thread_local_api_with_different_types() {
    let ptr_u64 = Box::into_raw(Box::new(42u64));
    let ptr_string = Box::into_raw(Box::new(String::from("hello")));
    let ptr_vec = Box::into_raw(Box::new(vec![1, 2, 3]));

    let atomic_u64 = AtomicPtr::new(ptr_u64);
    let atomic_string = AtomicPtr::new(ptr_string);
    let atomic_vec = AtomicPtr::new(ptr_vec);

    {
        let guard = protect(&atomic_u64).unwrap();
        assert_eq!(*guard, 42);
    }

    {
        let guard = protect(&atomic_string).unwrap();
        assert_eq!(&*guard, "hello");
    }

    {
        let guard = protect(&atomic_vec).unwrap();
        assert_eq!(guard.len(), 3);
    }

    let _ = unsafe { Box::from_raw(ptr_u64) };
    let _ = unsafe { Box::from_raw(ptr_string) };
    let _ = unsafe { Box::from_raw(ptr_vec) };
}

