mod domain;
mod hazard;
mod reclaim;
mod retired;
mod sync;
mod thread_local;

pub use domain::HazardDomain;
pub use hazard::{HazardGuard, HazardPointer};
pub use reclaim::{
    spawn_reclaim_task, spawn_reclaim_task_with_threshold, start_reclaim_task,
    start_reclaim_task_with_threshold,
};
pub use thread_local::{
    global_domain, init_global_domain, protect, protect_ptr, reclaim, retire, retire_with_deleter,
};

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    struct Node {
        value: usize,
        drop_counter: Arc<AtomicUsize>,
    }

    impl Drop for Node {
        fn drop(&mut self) {
            self.drop_counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_basic_protect_and_retire() {
        let domain = HazardDomain::new();
        let drop_counter = Arc::new(AtomicUsize::new(0));

        let node = Box::into_raw(Box::new(Node {
            value: 42,
            drop_counter: drop_counter.clone(),
        }));
        let atomic_ptr = AtomicPtr::new(node);

        let hp = HazardPointer::new(&domain);
        let guard = hp.protect(&atomic_ptr).unwrap();
        assert_eq!(guard.value, 42);

        let old_ptr = atomic_ptr.swap(std::ptr::null_mut(), Ordering::SeqCst);
        domain.retire(old_ptr);

        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 0);

        drop(guard);

        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_unprotected_immediate_reclaim() {
        let domain = HazardDomain::new();
        let drop_counter = Arc::new(AtomicUsize::new(0));

        let node = Box::into_raw(Box::new(Node {
            value: 100,
            drop_counter: drop_counter.clone(),
        }));

        domain.retire(node);
        assert_eq!(drop_counter.load(Ordering::SeqCst), 0);

        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_multiple_hazard_pointers() {
        let domain = HazardDomain::new();
        let drop_counter = Arc::new(AtomicUsize::new(0));

        let node1 = Box::into_raw(Box::new(Node {
            value: 1,
            drop_counter: drop_counter.clone(),
        }));
        let node2 = Box::into_raw(Box::new(Node {
            value: 2,
            drop_counter: drop_counter.clone(),
        }));

        let ptr1 = AtomicPtr::new(node1);
        let ptr2 = AtomicPtr::new(node2);

        let hp1 = HazardPointer::new(&domain);
        let hp2 = HazardPointer::new(&domain);

        let guard1 = hp1.protect(&ptr1).unwrap();
        let guard2 = hp2.protect(&ptr2).unwrap();

        assert_eq!(guard1.value, 1);
        assert_eq!(guard2.value, 2);

        let old1 = ptr1.swap(std::ptr::null_mut(), Ordering::SeqCst);
        let old2 = ptr2.swap(std::ptr::null_mut(), Ordering::SeqCst);

        domain.retire(old1);
        domain.retire(old2);

        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 0);

        drop(guard1);
        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 1);

        drop(guard2);
        domain.reclaim();
        assert_eq!(drop_counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_hazard_pointer_reuse() {
        let domain = HazardDomain::new();

        {
            let _hp1 = HazardPointer::new(&domain);
            let _hp2 = HazardPointer::new(&domain);
        }

        let _hp3 = HazardPointer::new(&domain);
        let _hp4 = HazardPointer::new(&domain);
    }

    #[test]
    fn test_null_pointer_protection() {
        let domain = HazardDomain::new();
        let atomic_ptr: AtomicPtr<Node> = AtomicPtr::new(std::ptr::null_mut());

        let hp = HazardPointer::new(&domain);
        let guard = hp.protect(&atomic_ptr);

        assert!(guard.is_none());
    }

    #[test]
    fn test_domain_drop_reclaims_all() {
        let drop_counter = Arc::new(AtomicUsize::new(0));

        {
            let domain = HazardDomain::new();

            for i in 0..10 {
                let node = Box::into_raw(Box::new(Node {
                    value: i,
                    drop_counter: drop_counter.clone(),
                }));
                domain.retire(node);
            }

            assert_eq!(drop_counter.load(Ordering::SeqCst), 0);
        }

        assert_eq!(drop_counter.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn test_concurrent_access() {
        let domain = Arc::new(HazardDomain::new());
        let drop_counter = Arc::new(AtomicUsize::new(0));
        let shared_ptr = Arc::new(AtomicPtr::new(Box::into_raw(Box::new(Node {
            value: 0,
            drop_counter: drop_counter.clone(),
        }))));

        let num_threads = 4;
        let iterations = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let domain = domain.clone();
                let drop_counter = drop_counter.clone();
                let shared_ptr = shared_ptr.clone();

                thread::spawn(move || {
                    for i in 0..iterations {
                        let hp = HazardPointer::new(&domain);

                        if let Some(guard) = hp.protect(&shared_ptr) {
                            let _ = guard.value;

                            if i % 10 == 0 {
                                let new_node = Box::into_raw(Box::new(Node {
                                    value: t * 1000 + i,
                                    drop_counter: drop_counter.clone(),
                                }));

                                let old = shared_ptr.swap(new_node, Ordering::SeqCst);
                                drop(guard);
                                domain.retire(old);
                            }
                        }

                        if i % 20 == 0 {
                            domain.reclaim();
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let final_ptr = shared_ptr.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !final_ptr.is_null() {
            domain.retire(final_ptr);
        }

        domain.reclaim();
    }

    #[test]
    fn test_auto_reclaim_threshold() {
        let domain = HazardDomain::with_threshold(5);
        let drop_counter = Arc::new(AtomicUsize::new(0));

        for i in 0..4 {
            let node = Box::into_raw(Box::new(Node {
                value: i,
                drop_counter: drop_counter.clone(),
            }));
            domain.retire(node);
        }

        assert_eq!(drop_counter.load(Ordering::SeqCst), 0);

        let node = Box::into_raw(Box::new(Node {
            value: 5,
            drop_counter: drop_counter.clone(),
        }));
        domain.retire(node);

        assert_eq!(drop_counter.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn test_background_reclaim_task() {
        use std::time::Duration;

        let domain = Arc::new(HazardDomain::with_threshold(1000));
        let drop_counter = Arc::new(AtomicUsize::new(0));

        for i in 0..10 {
            let node = Box::into_raw(Box::new(Node {
                value: i,
                drop_counter: drop_counter.clone(),
            }));
            domain.retire(node);
        }

        assert_eq!(drop_counter.load(Ordering::SeqCst), 0);

        let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(10));

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.abort();

        assert_eq!(drop_counter.load(Ordering::SeqCst), 10);
    }
}
