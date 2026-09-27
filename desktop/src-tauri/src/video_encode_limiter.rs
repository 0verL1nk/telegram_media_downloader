use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use sysinfo::System;
use tokio::sync::Notify;

const MAX_ENCODERS: usize = 4;
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const CPU_BUSY_THRESHOLD: f32 = 78.0;
const CPU_IDLE_THRESHOLD: f32 = 55.0;
const IDLE_SAMPLES_TO_SCALE_UP: usize = 3;

struct Inner {
    active: AtomicUsize,
    limit: AtomicUsize,
    max_limit: usize,
    changed: Notify,
}

/// Shared CPU-aware gate for manual, batch, and post-download video encoding.
pub(crate) struct VideoEncodeLimiter {
    inner: Arc<Inner>,
}

pub(crate) struct VideoEncodePermit {
    inner: Arc<Inner>,
}

impl VideoEncodeLimiter {
    pub(crate) fn start() -> Self {
        let max_limit = std::thread::available_parallelism()
            .map(|count| (count.get() / 4).clamp(1, MAX_ENCODERS))
            .unwrap_or(1);
        let inner = Arc::new(Inner {
            active: AtomicUsize::new(0),
            limit: AtomicUsize::new(1),
            max_limit,
            changed: Notify::new(),
        });
        spawn_cpu_monitor(Arc::clone(&inner));
        Self { inner }
    }

    pub(crate) async fn acquire(&self) -> VideoEncodePermit {
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let active = self.inner.active.load(Ordering::Acquire);
            let limit = self.inner.limit.load(Ordering::Acquire);
            if active < limit
                && self
                    .inner
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return VideoEncodePermit {
                    inner: Arc::clone(&self.inner),
                };
            }
            notified.await;
        }
    }
}

impl Drop for VideoEncodePermit {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        self.inner.changed.notify_waiters();
    }
}

fn spawn_cpu_monitor(inner: Arc<Inner>) {
    tokio::spawn(async move {
        let mut system = System::new();
        system.refresh_cpu_usage();
        tokio::time::sleep(CPU_SAMPLE_INTERVAL).await;

        let mut idle_samples = 0;
        loop {
            system.refresh_cpu_usage();
            let usage = system.global_cpu_usage().clamp(0.0, 100.0);
            let current = inner.limit.load(Ordering::Acquire);
            let next = if usage >= CPU_BUSY_THRESHOLD {
                idle_samples = 0;
                current.saturating_sub(1).max(1)
            } else if usage <= CPU_IDLE_THRESHOLD {
                idle_samples += 1;
                if idle_samples >= IDLE_SAMPLES_TO_SCALE_UP {
                    idle_samples = 0;
                    (current + 1).min(inner.max_limit)
                } else {
                    current
                }
            } else {
                idle_samples = 0;
                current
            };

            if next != current {
                inner.limit.store(next, Ordering::Release);
                inner.changed.notify_waiters();
            }
            tokio::time::sleep(CPU_SAMPLE_INTERVAL).await;
        }
    });
}
