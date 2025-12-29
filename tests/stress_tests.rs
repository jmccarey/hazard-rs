#![cfg(not(feature = "loom"))]

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use hazard::{HazardDomain, HazardPointer};

const NUM_THREADS: usize = 8;
const ITERATIONS_PER_THREAD: usize = 1000;

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
fn high_contention_single_pointer() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));
    let total_updates = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(0, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));
    let barrier = Arc::new(Barrier::new(NUM_THREADS));

    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|t| {
            let domain = domain.clone();
            let counter = counter.clone();
            let shared = shared.clone();
            let barrier = barrier.clone();
            let total_updates = total_updates.clone();

            thread::spawn(move || {
                barrier.wait();

                for i in 0..ITERATIONS_PER_THREAD {
                    let hp = HazardPointer::new(&domain);

                    if let Some(guard) = hp.protect(&shared) {
                        let _ = guard.value;

                        if i % 50 == t {
                            let new_ptr = DropTracker::boxed(
                                t * 10000 + i,
                                counter.clone(),
                            );

                            let old = shared.swap(new_ptr, Ordering::SeqCst);
                            drop(guard);
                            domain.retire(old);
                            total_updates.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    if i % 100 == 0 {
                        domain.reclaim();
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();

    let updates = total_updates.load(Ordering::Relaxed);
    let drops = counter.load(Ordering::SeqCst);
    assert_eq!(drops, updates + 1);
}

#[test]
fn slot_exhaustion_many_hazard_pointers() {
    let domain = Arc::new(HazardDomain::new());
    let barrier = Arc::new(Barrier::new(NUM_THREADS));

    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|_| {
            let domain = domain.clone();
            let barrier = barrier.clone();

            thread::spawn(move || {
                barrier.wait();

                let mut hps = Vec::with_capacity(100);
                for _ in 0..100 {
                    hps.push(HazardPointer::new(&domain));
                }

                thread::sleep(Duration::from_millis(10));

                drop(hps);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let hp = HazardPointer::new(&domain);
    let ptr = Box::into_raw(Box::new(42u64));
    let atomic = AtomicPtr::new(ptr);
    let guard = hp.protect(&atomic);
    assert!(guard.is_some());
    drop(guard);
    let _ = unsafe { Box::from_raw(ptr) };
}

#[test]
fn rapid_retire_reclaim_cycles() {
    let domain = Arc::new(HazardDomain::with_threshold(100));
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(NUM_THREADS + 1));

    let mut handles = vec![];

    for _ in 0..NUM_THREADS {
        let domain = domain.clone();
        let counter = counter.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for i in 0..ITERATIONS_PER_THREAD {
                domain.retire(DropTracker::boxed(i, counter.clone()));

                if i % 10 == 0 {
                    domain.reclaim();
                }
            }
        }));
    }

    {
        let domain = domain.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..ITERATIONS_PER_THREAD * 2 {
                domain.reclaim();
                thread::sleep(Duration::from_micros(100));
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    domain.reclaim();

    let expected = NUM_THREADS * ITERATIONS_PER_THREAD;
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}

#[test]
fn producer_consumer_pattern() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(0, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));
    let barrier = Arc::new(Barrier::new(NUM_THREADS));

    let num_producers = NUM_THREADS / 2;
    let num_consumers = NUM_THREADS - num_producers;

    let mut handles = vec![];

    for p in 0..num_producers {
        let domain = domain.clone();
        let counter = counter.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for i in 0..ITERATIONS_PER_THREAD {
                let new_ptr = DropTracker::boxed(p * 10000 + i, counter.clone());
                let old = shared.swap(new_ptr, Ordering::SeqCst);
                domain.retire(old);

                if i % 50 == 0 {
                    domain.reclaim();
                }
            }
        }));
    }

    for _ in 0..num_consumers {
        let domain = domain.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..ITERATIONS_PER_THREAD {
                let hp = HazardPointer::new(&domain);

                if let Some(guard) = hp.protect(&shared) {
                    let _ = guard.value;
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

    let expected = num_producers * ITERATIONS_PER_THREAD + 1;
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}

#[test]
fn long_running_concurrent_readers_writers() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let initial = DropTracker::boxed(0, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));
    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let stop = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for _ in 0..(NUM_THREADS - 2) {
        let domain = domain.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();
        let stop = stop.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            let mut reads = 0usize;
            while stop.load(Ordering::Relaxed) == 0 {
                let hp = HazardPointer::new(&domain);

                if let Some(guard) = hp.protect(&shared) {
                    let _ = guard.value;
                    reads += 1;
                }
            }
            reads
        }));
    }

    for w in 0..2 {
        let domain = domain.clone();
        let counter = counter.clone();
        let shared = shared.clone();
        let barrier = barrier.clone();
        let stop = stop.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            let mut writes = 0usize;
            while stop.load(Ordering::Relaxed) == 0 {
                let new_ptr = DropTracker::boxed(w * 100000 + writes, counter.clone());
                let old = shared.swap(new_ptr, Ordering::SeqCst);
                domain.retire(old);
                writes += 1;

                if writes % 100 == 0 {
                    domain.reclaim();
                }
            }
            writes
        }));
    }

    thread::sleep(Duration::from_millis(500));
    stop.store(1, Ordering::Relaxed);

    let mut total_reads = 0;
    let mut total_writes = 0;

    for (i, handle) in handles.into_iter().enumerate() {
        let count = handle.join().unwrap();
        if i < NUM_THREADS - 2 {
            total_reads += count;
        } else {
            total_writes += count;
        }
    }

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();

    assert!(total_reads > 0);
    assert!(total_writes > 0);

    let expected_drops = total_writes + 1;
    assert_eq!(counter.load(Ordering::SeqCst), expected_drops);
}

#[test]
fn memory_pressure_retire_faster_than_reclaim() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(NUM_THREADS + 1));

    let mut handles = vec![];

    for _ in 0..NUM_THREADS {
        let domain = domain.clone();
        let counter = counter.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for i in 0..ITERATIONS_PER_THREAD {
                let ptr = DropTracker::boxed(i, counter.clone());
                domain.retire(ptr);
            }
        }));
    }

    {
        let domain = domain.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for _ in 0..100 {
                thread::sleep(Duration::from_millis(5));
                domain.reclaim();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    domain.reclaim();

    let expected = NUM_THREADS * ITERATIONS_PER_THREAD;
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}

#[test]
fn aba_scenario_handling() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(3));

    let initial = DropTracker::boxed(1, counter.clone());
    let shared = Arc::new(AtomicPtr::new(initial));

    let reader_domain = domain.clone();
    let reader_shared = shared.clone();
    let reader_barrier = barrier.clone();

    let reader = thread::spawn(move || {
        reader_barrier.wait();

        for _ in 0..1000 {
            let hp = HazardPointer::new(&reader_domain);

            if let Some(guard) = hp.protect(&reader_shared) {
                let val = guard.value;
                assert!(val == 1 || val == 2);
            }
        }
    });

    let writer1_domain = domain.clone();
    let writer1_counter = counter.clone();
    let writer1_shared = shared.clone();
    let writer1_barrier = barrier.clone();

    let writer1 = thread::spawn(move || {
        writer1_barrier.wait();

        for _ in 0..500 {
            let new_ptr = DropTracker::boxed(2, writer1_counter.clone());
            let old = writer1_shared.swap(new_ptr, Ordering::SeqCst);
            writer1_domain.retire(old);
        }
    });

    let writer2_domain = domain.clone();
    let writer2_counter = counter.clone();
    let writer2_shared = shared.clone();
    let writer2_barrier = barrier.clone();

    let writer2 = thread::spawn(move || {
        writer2_barrier.wait();

        for _ in 0..500 {
            let new_ptr = DropTracker::boxed(1, writer2_counter.clone());
            let old = writer2_shared.swap(new_ptr, Ordering::SeqCst);
            writer2_domain.retire(old);
        }
    });

    reader.join().unwrap();
    writer1.join().unwrap();
    writer2.join().unwrap();

    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !final_ptr.is_null() {
        domain.retire(final_ptr);
    }
    domain.reclaim();

    let expected = 1000 + 1;
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}

#[test]
fn concurrent_domain_operations() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(4));

    let mut handles = vec![];

    {
        let domain = domain.clone();
        let counter = counter.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..1000 {
                domain.retire(DropTracker::boxed(i, counter.clone()));
            }
        }));
    }

    {
        let domain = domain.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..200 {
                domain.reclaim();
                thread::sleep(Duration::from_micros(50));
            }
        }));
    }

    {
        let domain = domain.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..500 {
                let _hp = HazardPointer::new(&domain);
                thread::sleep(Duration::from_micros(10));
            }
        }));
    }

    {
        let domain = domain.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..100 {
                let _ = domain.retired_count();
                thread::sleep(Duration::from_micros(100));
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 1000);
}

#[test]
fn stress_hazard_pointer_acquire_release() {
    let domain = Arc::new(HazardDomain::new());
    let barrier = Arc::new(Barrier::new(NUM_THREADS));

    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|_| {
            let domain = domain.clone();
            let barrier = barrier.clone();

            thread::spawn(move || {
                barrier.wait();

                for _ in 0..ITERATIONS_PER_THREAD {
                    let hp = HazardPointer::new(&domain);
                    drop(hp);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn interleaved_protect_retire_reclaim() {
    let domain = Arc::new(HazardDomain::new());
    let counter = Arc::new(AtomicUsize::new(0));

    let ptr1 = DropTracker::boxed(1, counter.clone());
    let ptr2 = DropTracker::boxed(2, counter.clone());
    let ptr3 = DropTracker::boxed(3, counter.clone());

    let atomic1 = Arc::new(AtomicPtr::new(ptr1));
    let atomic2 = Arc::new(AtomicPtr::new(ptr2));
    let atomic3 = Arc::new(AtomicPtr::new(ptr3));

    let barrier = Arc::new(Barrier::new(3));

    let t1 = {
        let domain = domain.clone();
        let atomic1 = atomic1.clone();
        let atomic2 = atomic2.clone();
        let barrier = barrier.clone();

        thread::spawn(move || {
            barrier.wait();

            for _ in 0..100 {
                let hp = HazardPointer::new(&domain);
                let _ = hp.protect(&atomic1);
                let _ = hp.protect(&atomic2);
            }
        })
    };

    let t2 = {
        let domain = domain.clone();
        let counter = counter.clone();
        let atomic1 = atomic1.clone();
        let atomic3 = atomic3.clone();
        let barrier = barrier.clone();

        thread::spawn(move || {
            barrier.wait();

            for i in 0..50 {
                let new_ptr = DropTracker::boxed(100 + i, counter.clone());
                let old = atomic1.swap(new_ptr, Ordering::SeqCst);
                domain.retire(old);

                let new_ptr = DropTracker::boxed(200 + i, counter.clone());
                let old = atomic3.swap(new_ptr, Ordering::SeqCst);
                domain.retire(old);
            }
        })
    };

    let t3 = {
        let domain = domain.clone();
        let barrier = barrier.clone();

        thread::spawn(move || {
            barrier.wait();

            for _ in 0..100 {
                domain.reclaim();
                thread::sleep(Duration::from_micros(100));
            }
        })
    };

    t1.join().unwrap();
    t2.join().unwrap();
    t3.join().unwrap();

    let p1 = atomic1.swap(std::ptr::null_mut(), Ordering::SeqCst);
    let p2 = atomic2.swap(std::ptr::null_mut(), Ordering::SeqCst);
    let p3 = atomic3.swap(std::ptr::null_mut(), Ordering::SeqCst);

    if !p1.is_null() {
        domain.retire(p1);
    }
    if !p2.is_null() {
        domain.retire(p2);
    }
    if !p3.is_null() {
        domain.retire(p3);
    }

    domain.reclaim();

    assert_eq!(counter.load(Ordering::SeqCst), 103);
}

