use std::sync::Arc;
use std::time::Duration;

use crate::domain::HazardDomain;

pub async fn start_reclaim_task(domain: Arc<HazardDomain>, interval: Duration) {
    let mut interval_timer = tokio::time::interval(interval);

    loop {
        interval_timer.tick().await;
        domain.reclaim();
    }
}

pub fn spawn_reclaim_task(domain: Arc<HazardDomain>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(start_reclaim_task(domain, interval))
}

pub async fn start_reclaim_task_with_threshold(
    domain: Arc<HazardDomain>,
    check_interval: Duration,
    threshold: usize,
) {
    let mut interval_timer = tokio::time::interval(check_interval);

    loop {
        interval_timer.tick().await;
        if domain.retired_count() >= threshold {
            domain.reclaim();
        }
    }
}

pub fn spawn_reclaim_task_with_threshold(
    domain: Arc<HazardDomain>,
    check_interval: Duration,
    threshold: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(start_reclaim_task_with_threshold(
        domain,
        check_interval,
        threshold,
    ))
}

