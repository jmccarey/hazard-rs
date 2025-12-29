#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use loom::sync::Arc;
use loom::thread;

use hazard::{HazardDomain, HazardPointer};

struct DropTracker {
    value: usize,
    counter: Arc<AtomicUsize>,
}

impl DropTracker {
    fn new(value: usize, counter: Arc<AtomicUsize>) -> *mut Self {
        Box::into_raw(Box::new(Self { value, counter }))
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn loom_basic_protect_retire() {
    loom::model(|| {
        let domain = Arc::new(HazardDomain::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let node = DropTracker::new(42, counter.clone());
        let shared = Arc::new(AtomicPtr::new(node));

        let hp = HazardPointer::new(&domain);
        let guard = hp.protect(&shared).unwrap();

        assert_eq!(guard.value, 42);

        let old = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
        domain.retire(old);

        domain.reclaim();
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        drop(guard);
        domain.reclaim();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn loom_concurrent_protect_and_reclaim() {
    loom::model(|| {
        let domain = Arc::new(HazardDomain::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let node = DropTracker::new(1, counter.clone());
        let shared = Arc::new(AtomicPtr::new(node));

        let domain2 = domain.clone();
        let shared2 = shared.clone();
        let counter2 = counter.clone();

        let reader = thread::spawn(move || {
            let hp = HazardPointer::new(&domain2);
            if let Some(guard) = hp.protect(&shared2) {
                let _ = guard.value;
            }
        });

        let domain3 = domain.clone();
        let shared3 = shared.clone();
        let writer = thread::spawn(move || {
            let new_node = DropTracker::new(2, counter2.clone());
            let old = shared3.swap(new_node, Ordering::SeqCst);
            if !old.is_null() {
                domain3.retire(old);
            }
            domain3.reclaim();
        });

        reader.join().unwrap();
        writer.join().unwrap();

        let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !final_ptr.is_null() {
            domain.retire(final_ptr);
        }
        domain.reclaim();
    });
}

#[test]
fn loom_multiple_protectors() {
    loom::model(|| {
        let domain = Arc::new(HazardDomain::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let node = DropTracker::new(0, counter.clone());
        let shared = Arc::new(AtomicPtr::new(node));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let domain = domain.clone();
                let shared = shared.clone();

                thread::spawn(move || {
                    let hp = HazardPointer::new(&domain);
                    if let Some(guard) = hp.protect(&shared) {
                        let _ = guard.value;
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !final_ptr.is_null() {
            domain.retire(final_ptr);
        }
        domain.reclaim();
    });
}

#[test]
fn loom_protect_during_swap() {
    loom::model(|| {
        let domain = Arc::new(HazardDomain::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let node = DropTracker::new(1, counter.clone());
        let shared = Arc::new(AtomicPtr::new(node));

        let domain2 = domain.clone();
        let shared2 = shared.clone();

        let protector = thread::spawn(move || {
            let hp = HazardPointer::new(&domain2);
            let _ = hp.protect(&shared2);
        });

        let new_node = DropTracker::new(2, counter.clone());
        let old = shared.swap(new_node, Ordering::SeqCst);
        if !old.is_null() {
            domain.retire(old);
        }

        protector.join().unwrap();

        let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !final_ptr.is_null() {
            domain.retire(final_ptr);
        }
        domain.reclaim();
    });
}

#[test]
fn loom_retire_protected_not_reclaimed() {
    loom::model(|| {
        let domain = Arc::new(HazardDomain::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let node = DropTracker::new(42, counter.clone());
        let shared = Arc::new(AtomicPtr::new(node));

        let domain2 = domain.clone();
        let shared2 = shared.clone();

        let protector = thread::spawn(move || {
            let hp = HazardPointer::new(&domain2);
            let guard = hp.protect(&shared2);
            loom::thread::yield_now();
            drop(guard);
        });

        loom::thread::yield_now();

        let old = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !old.is_null() {
            domain.retire(old);
        }
        domain.reclaim();

        protector.join().unwrap();
        domain.reclaim();

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    });
}

