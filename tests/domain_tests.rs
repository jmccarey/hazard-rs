#![cfg(not(feature = "loom"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use hazard::{HazardDomain, HazardPointer};

struct DropTracker {
    counter: Arc<AtomicUsize>,
}

impl DropTracker {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        Self { counter }
    }

    fn boxed(counter: Arc<AtomicUsize>) -> *mut Self {
        Box::into_raw(Box::new(Self::new(counter)))
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn new_creates_domain_with_default_threshold() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..63 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 64);
}

#[test]
fn with_threshold_respects_custom_threshold() {
    let domain = HazardDomain::with_threshold(10);
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..9 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 10);
}

#[test]
fn retire_with_custom_deleter() {
    static CUSTOM_DELETE_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn custom_deleter(ptr: *mut u8) {
        CUSTOM_DELETE_COUNT.fetch_add(1, Ordering::SeqCst);
        let _ = unsafe { Box::from_raw(ptr as *mut u64) };
    }

    let domain = HazardDomain::new();
    let ptr = Box::into_raw(Box::new(42u64));

    domain.retire_with_deleter(ptr, custom_deleter);
    domain.reclaim();

    assert_eq!(CUSTOM_DELETE_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn retire_properly_frees_allocation() {
    let counter = Arc::new(AtomicUsize::new(0));
    let domain = HazardDomain::new();

    let ptr = DropTracker::boxed(counter.clone());
    domain.retire(ptr);

    assert_eq!(counter.load(Ordering::SeqCst), 0);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn reclaim_frees_unreferenced_preserves_protected() {
    use std::sync::atomic::AtomicPtr;

    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let protected_ptr = DropTracker::boxed(counter.clone());
    let unprotected_ptr = DropTracker::boxed(counter.clone());

    let atomic = AtomicPtr::new(protected_ptr);
    let hp = HazardPointer::new(&domain);
    let _guard = hp.protect(&atomic).unwrap();

    domain.retire(protected_ptr);
    domain.retire(unprotected_ptr);

    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);

    drop(_guard);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn retired_count_accuracy() {
    let domain = HazardDomain::with_threshold(1000);
    let counter = Arc::new(AtomicUsize::new(0));

    assert_eq!(domain.retired_count(), 0);

    for i in 0..50 {
        domain.retire(DropTracker::boxed(counter.clone()));
        assert_eq!(domain.retired_count(), i + 1);
    }

    domain.reclaim();
    assert_eq!(domain.retired_count(), 0);
}

#[test]
fn domain_drop_reclaims_all_pending() {
    let counter = Arc::new(AtomicUsize::new(0));

    {
        let domain = HazardDomain::with_threshold(1000);
        for _ in 0..100 {
            domain.retire(DropTracker::boxed(counter.clone()));
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    assert_eq!(counter.load(Ordering::SeqCst), 100);
}

#[test]
fn zero_threshold_immediate_reclaim() {
    let domain = HazardDomain::with_threshold(0);
    let counter = Arc::new(AtomicUsize::new(0));

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[test]
fn multiple_domains_operate_independently() {
    let counter1 = Arc::new(AtomicUsize::new(0));
    let counter2 = Arc::new(AtomicUsize::new(0));

    let domain1 = HazardDomain::with_threshold(100);
    let domain2 = HazardDomain::with_threshold(100);

    for _ in 0..10 {
        domain1.retire(DropTracker::boxed(counter1.clone()));
    }

    for _ in 0..20 {
        domain2.retire(DropTracker::boxed(counter2.clone()));
    }

    assert_eq!(domain1.retired_count(), 10);
    assert_eq!(domain2.retired_count(), 20);

    domain1.reclaim();

    assert_eq!(counter1.load(Ordering::SeqCst), 10);
    assert_eq!(counter2.load(Ordering::SeqCst), 0);

    domain2.reclaim();

    assert_eq!(counter1.load(Ordering::SeqCst), 10);
    assert_eq!(counter2.load(Ordering::SeqCst), 20);
}

#[test]
fn default_trait_implementation() {
    let domain: HazardDomain = Default::default();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..63 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    domain.retire(DropTracker::boxed(counter.clone()));
    assert_eq!(counter.load(Ordering::SeqCst), 64);
}

#[test]
fn reclaim_with_no_retired_objects() {
    let domain = HazardDomain::new();
    domain.reclaim();
    assert_eq!(domain.retired_count(), 0);
}

#[test]
fn multiple_reclaim_calls_are_idempotent() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..10 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 10);

    domain.reclaim();
    domain.reclaim();
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 10);
    assert_eq!(domain.retired_count(), 0);
}

