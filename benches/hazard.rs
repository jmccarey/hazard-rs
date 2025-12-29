use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hazard::{global_domain, protect, retire, HazardDomain, HazardPointer};

fn bench_acquisition(c: &mut Criterion) {
    let mut group = c.benchmark_group("acquisition");

    group.bench_function("fresh_slot", |b| {
        b.iter(|| {
            let domain = HazardDomain::new();
            let hp = HazardPointer::new(&domain);
            black_box(hp);
        });
    });

    group.bench_function("slot_reuse", |b| {
        let domain = HazardDomain::new();
        b.iter(|| {
            let hp = HazardPointer::new(&domain);
            black_box(&hp);
            drop(hp);
        });
    });

    group.bench_function("multiple_slots", |b| {
        let domain = HazardDomain::new();
        b.iter(|| {
            let hp1 = HazardPointer::new(&domain);
            let hp2 = HazardPointer::new(&domain);
            let hp3 = HazardPointer::new(&domain);
            let hp4 = HazardPointer::new(&domain);
            black_box((&hp1, &hp2, &hp3, &hp4));
        });
    });

    group.finish();
}

fn bench_protection(c: &mut Criterion) {
    let mut group = c.benchmark_group("protection");

    group.bench_function("protect", |b| {
        let domain = HazardDomain::new();
        let hp = HazardPointer::new(&domain);
        let ptr = AtomicPtr::new(Box::into_raw(Box::new(42u64)));

        b.iter(|| {
            let guard = hp.protect(&ptr);
            black_box(&guard);
        });

        let raw = ptr.load(Ordering::SeqCst);
        if !raw.is_null() {
            let _ = unsafe { Box::from_raw(raw) };
        }
    });

    group.bench_function("protect_ptr", |b| {
        let domain = HazardDomain::new();
        let hp = HazardPointer::new(&domain);
        let raw = Box::into_raw(Box::new(42u64));

        b.iter(|| {
            let guard = hp.protect_ptr(raw);
            black_box(&guard);
        });

        let _ = unsafe { Box::from_raw(raw) };
    });

    group.bench_function("protect_null", |b| {
        let domain = HazardDomain::new();
        let hp = HazardPointer::new(&domain);
        let ptr: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());

        b.iter(|| {
            let guard = hp.protect(&ptr);
            black_box(&guard);
        });
    });

    group.finish();
}

fn bench_retirement(c: &mut Criterion) {
    let mut group = c.benchmark_group("retirement");

    group.bench_function("single_retire", |b| {
        let domain = HazardDomain::with_threshold(usize::MAX);
        b.iter(|| {
            let ptr = Box::into_raw(Box::new(42u64));
            domain.retire(ptr);
        });
        domain.reclaim();
    });

    for count in [100, 1000] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("batch_retire", count), &count, |b, &n| {
            b.iter(|| {
                let domain = HazardDomain::with_threshold(usize::MAX);
                for _ in 0..n {
                    let ptr = Box::into_raw(Box::new(42u64));
                    domain.retire(ptr);
                }
                black_box(domain.retired_count());
            });
        });
    }

    group.finish();
}

fn bench_reclamation(c: &mut Criterion) {
    let mut group = c.benchmark_group("reclamation");

    for count in [0, 10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("reclaim_unprotected", count),
            &count,
            |b, &n| {
                b.iter_batched(
                    || {
                        let domain = HazardDomain::with_threshold(usize::MAX);
                        for _ in 0..n {
                            let ptr = Box::into_raw(Box::new(42u64));
                            domain.retire(ptr);
                        }
                        domain
                    },
                    |domain| {
                        domain.reclaim();
                        black_box(domain.retired_count());
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.bench_function("reclaim_with_protection", |b| {
        b.iter_batched(
            || {
                let domain = Arc::new(HazardDomain::with_threshold(usize::MAX));
                let protected_ptr = Box::into_raw(Box::new(42u64));

                for _ in 0..100 {
                    let ptr = Box::into_raw(Box::new(42u64));
                    domain.retire(ptr);
                }
                domain.retire(protected_ptr);

                (domain, protected_ptr)
            },
            |(domain, protected_ptr)| {
                let hp = HazardPointer::new(&domain);
                let atomic = AtomicPtr::new(protected_ptr);
                let guard = hp.protect(&atomic);

                domain.reclaim();
                let remaining = domain.retired_count();
                drop(guard);
                domain.reclaim();
                black_box(remaining);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_thread_local(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_local");

    group.bench_function("thread_local_protect", |b| {
        let ptr = AtomicPtr::new(Box::into_raw(Box::new(42u64)));

        b.iter(|| {
            let guard = protect(&ptr);
            black_box(&guard);
        });

        let raw = ptr.load(Ordering::SeqCst);
        if !raw.is_null() {
            let _ = unsafe { Box::from_raw(raw) };
        }
    });

    group.bench_function("thread_local_retire", |b| {
        b.iter(|| {
            let ptr = Box::into_raw(Box::new(42u64));
            retire(ptr);
        });
        global_domain().reclaim();
    });

    group.bench_function("explicit_vs_thread_local", |b| {
        let domain = HazardDomain::new();
        let hp = HazardPointer::new(&domain);
        let ptr = AtomicPtr::new(Box::into_raw(Box::new(42u64)));

        b.iter(|| {
            let guard = hp.protect(&ptr);
            black_box(&guard);
        });

        let raw = ptr.load(Ordering::SeqCst);
        if !raw.is_null() {
            let _ = unsafe { Box::from_raw(raw) };
        }
    });

    group.finish();
}

fn bench_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("contention");
    group.sample_size(50);

    for num_threads in [1, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("read_heavy", num_threads),
            &num_threads,
            |b, &n| {
                b.iter_custom(|iters| {
                    let domain = Arc::new(HazardDomain::with_threshold(1000));
                    let shared = Arc::new(AtomicPtr::new(Box::into_raw(Box::new(0u64))));
                    let barrier = Arc::new(Barrier::new(n));
                    let total_ops = Arc::new(AtomicUsize::new(0));

                    let handles: Vec<_> = (0..n)
                        .map(|id| {
                            let domain = domain.clone();
                            let shared = shared.clone();
                            let barrier = barrier.clone();
                            let total_ops = total_ops.clone();

                            thread::spawn(move || {
                                let hp = HazardPointer::new(&domain);
                                barrier.wait();

                                let ops_per_thread = iters as usize / n;
                                for i in 0..ops_per_thread {
                                    if i % 10 == 0 && id == 0 {
                                        let new_ptr = Box::into_raw(Box::new(i as u64));
                                        let old = shared.swap(new_ptr, Ordering::SeqCst);
                                        if !old.is_null() {
                                            domain.retire(old);
                                        }
                                    } else if let Some(guard) = hp.protect(&shared) {
                                        black_box(*guard);
                                    }
                                }

                                total_ops.fetch_add(ops_per_thread, Ordering::Relaxed);
                            })
                        })
                        .collect();

                    let start = std::time::Instant::now();
                    for handle in handles {
                        handle.join().unwrap();
                    }
                    let elapsed = start.elapsed();

                    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
                    if !final_ptr.is_null() {
                        domain.retire(final_ptr);
                    }
                    domain.reclaim();

                    elapsed
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("write_heavy", num_threads),
            &num_threads,
            |b, &n| {
                b.iter_custom(|iters| {
                    let domain = Arc::new(HazardDomain::with_threshold(1000));
                    let shared = Arc::new(AtomicPtr::new(Box::into_raw(Box::new(0u64))));
                    let barrier = Arc::new(Barrier::new(n));

                    let handles: Vec<_> = (0..n)
                        .map(|_| {
                            let domain = domain.clone();
                            let shared = shared.clone();
                            let barrier = barrier.clone();

                            thread::spawn(move || {
                                let hp = HazardPointer::new(&domain);
                                barrier.wait();

                                let ops_per_thread = iters as usize / n;
                                for i in 0..ops_per_thread {
                                    if i % 2 == 0 {
                                        let new_ptr = Box::into_raw(Box::new(i as u64));
                                        let old = shared.swap(new_ptr, Ordering::SeqCst);
                                        if !old.is_null() {
                                            domain.retire(old);
                                        }
                                    } else if let Some(guard) = hp.protect(&shared) {
                                        black_box(*guard);
                                    }
                                }
                            })
                        })
                        .collect();

                    let start = std::time::Instant::now();
                    for handle in handles {
                        handle.join().unwrap();
                    }
                    let elapsed = start.elapsed();

                    let final_ptr = shared.swap(std::ptr::null_mut(), Ordering::SeqCst);
                    if !final_ptr.is_null() {
                        domain.retire(final_ptr);
                    }
                    domain.reclaim();

                    elapsed
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_acquisition,
    bench_protection,
    bench_retirement,
    bench_reclamation,
    bench_thread_local,
    bench_contention,
);

criterion_main!(benches);

