#![cfg(not(feature = "loom"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hazard::{
    spawn_reclaim_task, spawn_reclaim_task_with_threshold, start_reclaim_task,
    start_reclaim_task_with_threshold, HazardDomain,
};

struct DropTracker {
    counter: Arc<AtomicUsize>,
}

impl DropTracker {
    fn boxed(counter: Arc<AtomicUsize>) -> *mut Self {
        Box::into_raw(Box::new(Self { counter }))
    }
}

impl Drop for DropTracker {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn start_reclaim_task_runs_periodic_reclamation() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..10 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    assert_eq!(counter.load(Ordering::SeqCst), 0);

    let domain_clone = domain.clone();
    let handle = tokio::spawn(async move {
        tokio::select! {
            _ = start_reclaim_task(domain_clone, Duration::from_millis(10)) => {}
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 10);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spawn_reclaim_task_returns_abortable_handle() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..5 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(10));

    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 5);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn start_reclaim_task_with_threshold_respects_threshold() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    let domain_clone = domain.clone();
    let handle = tokio::spawn(async move {
        tokio::select! {
            _ = start_reclaim_task_with_threshold(
                domain_clone,
                Duration::from_millis(10),
                5
            ) => {}
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(counter.load(Ordering::SeqCst), 0);

    for _ in 0..5 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 8);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spawn_reclaim_task_with_threshold_conditional_reclaim() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task_with_threshold(domain.clone(), Duration::from_millis(10), 10);

    for _ in 0..5 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    for _ in 0..10 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.abort();

    assert!(counter.load(Ordering::SeqCst) >= 10);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn task_cancellation_stops_reclamation() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(5));

    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.abort();

    let count_at_abort = counter.load(Ordering::SeqCst);

    for _ in 0..10 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(counter.load(Ordering::SeqCst), count_at_abort);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn multiple_concurrent_tasks_on_same_domain() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle1 = spawn_reclaim_task(domain.clone(), Duration::from_millis(10));
    let handle2 = spawn_reclaim_task(domain.clone(), Duration::from_millis(15));
    let handle3 = spawn_reclaim_task(domain.clone(), Duration::from_millis(20));

    for i in 0..30 {
        domain.retire(DropTracker::boxed(counter.clone()));
        if i % 5 == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    handle1.abort();
    handle2.abort();
    handle3.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 30);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reclaim_task_with_zero_threshold() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task_with_threshold(domain.clone(), Duration::from_millis(10), 0);

    domain.retire(DropTracker::boxed(counter.clone()));
    tokio::time::sleep(Duration::from_millis(30)).await;

    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rapid_retire_during_reclaim_task() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(5));

    for _ in 0..100 {
        domain.retire(DropTracker::boxed(counter.clone()));
        tokio::task::yield_now().await;
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 100);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reclaim_task_handles_empty_retired_list() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));

    let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(10));

    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.abort();

    assert_eq!(domain.retired_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(miri, ignore)]
async fn reclaim_task_with_concurrent_operations() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task(domain.clone(), Duration::from_millis(10));

    let mut tasks = vec![];

    for _ in 0..4 {
        let domain = domain.clone();
        let counter = counter.clone();

        tasks.push(tokio::spawn(async move {
            for _ in 0..25 {
                domain.retire(DropTracker::boxed(counter.clone()));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }));
    }

    for task in tasks {
        task.await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 100);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn threshold_boundary_condition() {
    let domain = Arc::new(HazardDomain::with_threshold(1000));
    let counter = Arc::new(AtomicUsize::new(0));

    let handle = spawn_reclaim_task_with_threshold(domain.clone(), Duration::from_millis(10), 5);

    for _ in 0..4 {
        domain.retire(DropTracker::boxed(counter.clone()));
    }

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    domain.retire(DropTracker::boxed(counter.clone()));

    tokio::time::sleep(Duration::from_millis(30)).await;
    handle.abort();

    assert_eq!(counter.load(Ordering::SeqCst), 5);
}

