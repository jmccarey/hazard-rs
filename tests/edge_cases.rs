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
fn protect_pointer_changes_during_protect_loop() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(0, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));
    let barrier = Arc::new(Barrier::new(2));

    let writer_domain = domain.clone();
    let writer_counter = counter.clone();
    let writer_shared = shared.clone();
    let writer_barrier = barrier.clone();

    let writer = thread::spawn(move || {
        writer_barrier.wait();

        for i in 1..=1000 {
            let new_ptr = DropTracker::boxed(i, writer_counter.clone());
            let old = writer_shared.swap(new_ptr, Ordering::SeqCst);
            writer_domain.retire(old);
        }
    });

    let reader_domain = domain.clone();
    let reader_shared = shared.clone();
    let reader_barrier = barrier.clone();

    let reader = thread::spawn(move || {
        reader_barrier.wait();

        let hp = HazardPointer::new(&reader_domain);

        for _ in 0..1000 {
            if let Some(guard) = hp.protect(&reader_shared) {
                let val = guard.value;
                assert!(val <= 1000);
            }
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();
}

#[test]
fn large_object_retirement_and_reclamation() {
    #[repr(align(4096))]
    struct LargeObject {
        _data: [u8; 65536],
        counter: Arc<AtomicUsize>,
    }

    impl Drop for LargeObject {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..10 {
        let obj = Box::into_raw(Box::new(LargeObject {
            _data: [0u8; 65536],
            counter: counter.clone(),
        }));
        domain.retire(obj);
    }

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 10);
}

#[test]
fn zero_sized_type_protection() {
    struct ZeroSized;

    impl Drop for ZeroSized {
        fn drop(&mut self) {}
    }

    let domain = HazardDomain::new();

    let ptr = Box::into_raw(Box::new(ZeroSized));
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic);

    assert!(guard.is_some());

    drop(guard);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn drop_order_multiple_guards() {
    let domain = HazardDomain::new();
    let order = Arc::new(AtomicUsize::new(0));

    struct OrderTracker {
        id: usize,
        order: Arc<AtomicUsize>,
        drop_order: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl Drop for OrderTracker {
        fn drop(&mut self) {
            let mut vec = self.drop_order.lock().unwrap();
            vec.push(self.id);
            self.order.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drop_order = Arc::new(std::sync::Mutex::new(Vec::new()));

    let ptr1 = Box::into_raw(Box::new(OrderTracker {
        id: 1,
        order: order.clone(),
        drop_order: drop_order.clone(),
    }));
    let ptr2 = Box::into_raw(Box::new(OrderTracker {
        id: 2,
        order: order.clone(),
        drop_order: drop_order.clone(),
    }));
    let ptr3 = Box::into_raw(Box::new(OrderTracker {
        id: 3,
        order: order.clone(),
        drop_order: drop_order.clone(),
    }));

    let atomic1 = AtomicPtr::new(ptr1);
    let atomic2 = AtomicPtr::new(ptr2);
    let atomic3 = AtomicPtr::new(ptr3);

    let hp1 = HazardPointer::new(&domain);
    let hp2 = HazardPointer::new(&domain);
    let hp3 = HazardPointer::new(&domain);

    let guard1 = hp1.protect(&atomic1).unwrap();
    let guard2 = hp2.protect(&atomic2).unwrap();
    let guard3 = hp3.protect(&atomic3).unwrap();

    assert_eq!(guard1.id, 1);
    assert_eq!(guard2.id, 2);
    assert_eq!(guard3.id, 3);

    domain.retire(ptr1);
    domain.retire(ptr2);
    domain.retire(ptr3);

    domain.reclaim();
    assert_eq!(order.load(Ordering::SeqCst), 0);

    drop(guard2);
    domain.reclaim();
    assert_eq!(order.load(Ordering::SeqCst), 1);
    assert_eq!(drop_order.lock().unwrap()[0], 2);

    drop(guard1);
    drop(guard3);
    domain.reclaim();
    assert_eq!(order.load(Ordering::SeqCst), 3);
}

#[test]
fn alignment_edge_cases() {
    #[repr(align(1))]
    struct Align1(Arc<AtomicUsize>);

    #[repr(align(8))]
    struct Align8(Arc<AtomicUsize>);

    #[repr(align(64))]
    struct Align64(Arc<AtomicUsize>);

    #[repr(align(256))]
    struct Align256(Arc<AtomicUsize>);

    impl Drop for Align1 {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for Align8 {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for Align64 {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for Align256 {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let p1 = Box::into_raw(Box::new(Align1(counter.clone())));
    let p8 = Box::into_raw(Box::new(Align8(counter.clone())));
    let p64 = Box::into_raw(Box::new(Align64(counter.clone())));
    let p256 = Box::into_raw(Box::new(Align256(counter.clone())));

    let a1 = AtomicPtr::new(p1);
    let a8 = AtomicPtr::new(p8);
    let a64 = AtomicPtr::new(p64);
    let a256 = AtomicPtr::new(p256);

    let hp = HazardPointer::new(&domain);

    let g1 = hp.protect(&a1);
    assert!(g1.is_some());
    drop(g1);

    let g8 = hp.protect(&a8);
    assert!(g8.is_some());
    drop(g8);

    let g64 = hp.protect(&a64);
    assert!(g64.is_some());
    drop(g64);

    let g256 = hp.protect(&a256);
    assert!(g256.is_some());
    drop(g256);

    domain.retire(p1);
    domain.retire(p8);
    domain.retire(p64);
    domain.retire(p256);

    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 4);
}

#[test]
fn very_high_threshold_domain() {
    let domain = HazardDomain::with_threshold(usize::MAX);
    let counter = Arc::new(AtomicUsize::new(0));

    for i in 0..100 {
        domain.retire(DropTracker::boxed(i, counter.clone()));
    }

    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert_eq!(domain.retired_count(), 100);

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 100);
}

#[test]
fn protect_immediately_after_clear() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr1 = DropTracker::boxed(1, counter.clone());
    let ptr2 = DropTracker::boxed(2, counter.clone());

    let atomic1 = AtomicPtr::new(ptr1);
    let atomic2 = AtomicPtr::new(ptr2);

    let hp = HazardPointer::new(&domain);

    let guard1 = hp.protect(&atomic1).unwrap();
    assert_eq!(guard1.value, 1);

    hp.clear();

    let guard2 = hp.protect(&atomic2).unwrap();
    assert_eq!(guard2.value, 2);

    domain.retire(ptr1);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    drop(guard1);
    drop(guard2);

    domain.retire(ptr2);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn nested_data_structures() {
    struct Node {
        value: i32,
        children: Vec<Box<Node>>,
        counter: Arc<AtomicUsize>,
    }

    impl Drop for Node {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let leaf1 = Node {
        value: 1,
        children: vec![],
        counter: counter.clone(),
    };

    let leaf2 = Node {
        value: 2,
        children: vec![],
        counter: counter.clone(),
    };

    let root = Box::into_raw(Box::new(Node {
        value: 0,
        children: vec![Box::new(leaf1), Box::new(leaf2)],
        counter: counter.clone(),
    }));

    let atomic = AtomicPtr::new(root);
    let hp = HazardPointer::new(&domain);

    let guard = hp.protect(&atomic).unwrap();
    assert_eq!(guard.value, 0);
    assert_eq!(guard.children.len(), 2);
    assert_eq!(guard.children[0].value, 1);
    assert_eq!(guard.children[1].value, 2);

    drop(guard);

    domain.retire(root);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[test]
fn protect_ptr_then_protect_atomic() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr1 = DropTracker::boxed(1, counter.clone());
    let ptr2 = DropTracker::boxed(2, counter.clone());

    let atomic = AtomicPtr::new(ptr2);

    let hp = HazardPointer::new(&domain);

    let guard1 = hp.protect_ptr(ptr1).unwrap();
    assert_eq!(guard1.value, 1);

    drop(guard1);

    let guard2 = hp.protect(&atomic).unwrap();
    assert_eq!(guard2.value, 2);

    domain.retire(ptr1);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    drop(guard2);
    domain.retire(ptr2);
    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn multiple_sequential_protections_same_hp() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let hp = HazardPointer::new(&domain);

    for i in 0..100 {
        let ptr = DropTracker::boxed(i, counter.clone());
        let atomic = AtomicPtr::new(ptr);

        let guard = hp.protect(&atomic).unwrap();
        assert_eq!(guard.value, i);

        drop(guard);
        domain.retire(ptr);
    }

    domain.reclaim();
    assert_eq!(counter.load(Ordering::SeqCst), 100);
}

#[test]
fn retire_during_active_protection() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let guard = hp.protect(&atomic).unwrap();

    domain.retire(ptr);
    domain.reclaim();
    domain.reclaim();
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert_eq!(guard.value, 42);

    drop(guard);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn domain_with_only_protected_objects() {
    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(1, counter.clone());
    let atomic = AtomicPtr::new(ptr);

    let hp = HazardPointer::new(&domain);
    let _guard = hp.protect(&atomic).unwrap();

    domain.retire(ptr);

    for _ in 0..10 {
        domain.reclaim();
    }

    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert_eq!(domain.retired_count(), 1);
}

#[test]
fn thread_panics_during_protection() {
    use std::panic;

    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = DropTracker::boxed(42, counter.clone());
    let atomic = Arc::new(AtomicPtr::new(ptr));

    let domain_clone = domain.clone();
    let atomic_clone = atomic.clone();

    let result = panic::catch_unwind(move || {
        let hp = HazardPointer::new(&domain_clone);
        let _guard = hp.protect(&atomic_clone).unwrap();

        panic!("intentional panic");
    });

    assert!(result.is_err());

    domain.retire(ptr);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_protection_of_changing_pointer() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(0, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));
    let barrier = Arc::new(Barrier::new(5));

    let mut handles = vec![];

    for _ in 0..4 {
        let domain = domain.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..500 {
                let hp = HazardPointer::new(&domain);
                if let Some(guard) = hp.protect(&shared) {
                    let _ = guard.value;
                    std::hint::spin_loop();
                }
            }
        }));
    }

    {
        let domain = domain.clone();
        let counter = counter.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for i in 1..=200 {
                let new_ptr = DropTracker::boxed(i, counter.clone());
                let old = shared.swap(new_ptr, Ordering::SeqCst);
                domain.retire(old);

                if i % 20 == 0 {
                    domain.reclaim();
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 201);
}

#[test]
fn protection_with_interior_mutability() {
    use std::cell::Cell;

    struct MutableData {
        value: Cell<usize>,
        counter: Arc<AtomicUsize>,
    }

    impl Drop for MutableData {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    let domain = HazardDomain::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr = Box::into_raw(Box::new(MutableData {
        value: Cell::new(0),
        counter: counter.clone(),
    }));

    let atomic = AtomicPtr::new(ptr);
    let hp = HazardPointer::new(&domain);

    let guard = hp.protect(&atomic).unwrap();
    assert_eq!(guard.value.get(), 0);

    guard.value.set(42);
    assert_eq!(guard.value.get(), 42);

    drop(guard);
    domain.retire(ptr);
    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

