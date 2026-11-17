//! The CPU and memory feed: one worker thread, one sampler, one message per sample.
//!
//! The sampler is owned by the worker and never leaves it. What crosses to the UI thread is a pair of
//! percentages packed into a `PostMessage`, and what the IPC server reads is a handful of atomics —
//! so a slow Win32 counter can never make the clock stutter, and a bar that repaints never waits for
//! a sample.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use neb_core::metrics::{Sample, Sampler};

use crate::widgets::metrics::{Reading, UNKNOWN};

/// How often a sample is taken, matching the C# bar.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How long the worker sleeps at a time while it waits, so stopping it is quick.
const TICK: Duration = Duration::from_millis(50);

/// Everything the rest of the process may ask about the machine's load.
#[derive(Debug)]
pub struct Metrics {
    /// CPU busy share, `0..=100`, or [`UNKNOWN`].
    cpu_percent: AtomicU32,
    /// Memory in use as a percentage, or [`UNKNOWN`].
    ram_percent: AtomicU32,
    /// Memory in use in mebibytes.
    ram_mb: AtomicU32,
    running: AtomicBool,
    targets: Mutex<Vec<isize>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// One reading, in every unit the rest of the process needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// CPU busy share, `0.0..=100.0`.
    pub cpu: f32,
    /// Memory in use, in mebibytes.
    pub ram_mb: u32,
    /// What the bar draws: both percentages, rounded, or `None` for a reading that failed.
    pub reading: Reading,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            cpu: 0.0,
            ram_mb: 0,
            reading: Reading {
                cpu: Some(0),
                ram: Some(0),
            },
        }
    }
}

impl Metrics {
    /// Starts the worker with no bars to notify.
    pub fn start() -> Arc<Self> {
        let metrics = Arc::new(Self {
            cpu_percent: AtomicU32::new(0),
            ram_percent: AtomicU32::new(UNKNOWN),
            ram_mb: AtomicU32::new(0),
            running: AtomicBool::new(true),
            targets: Mutex::new(Vec::new()),
            worker: Mutex::new(None),
        });

        let worker = {
            let metrics = Arc::clone(&metrics);
            thread::Builder::new()
                .name("neb-metrics".to_owned())
                .spawn(move || metrics.sample_forever())
        };

        if let Ok(handle) = worker {
            *metrics
                .worker
                .lock()
                .unwrap_or_else(|lock| lock.into_inner()) = Some(handle);
        }

        metrics
    }

    /// Replaces the windows a sample is posted to.
    pub fn set_targets(&self, bars: &[isize]) {
        let mut targets = self.targets.lock().unwrap_or_else(|lock| lock.into_inner());
        targets.clear();
        targets.extend_from_slice(bars);
    }

    /// The latest reading, for the IPC surface.
    pub fn snapshot(&self) -> Snapshot {
        let cpu = self.cpu_percent.load(Ordering::Relaxed);
        let ram = self.ram_percent.load(Ordering::Relaxed);

        Snapshot {
            cpu: f32::from(u16::try_from(cpu).unwrap_or(0)),
            ram_mb: self.ram_mb.load(Ordering::Relaxed),
            reading: Reading {
                cpu: reading(cpu),
                ram: reading(ram),
            },
        }
    }

    /// Stops the worker and waits for it, at most one sleep tick after the request.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);

        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .take();

        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    /// The worker's whole life: prime the sampler, then publish a reading every [`INTERVAL`].
    fn sample_forever(&self) {
        let mut sampler = Sampler::new();

        // The first reading has no previous totals to compare against, so it only establishes them
        // and reports the memory state; the CPU percentage starts with the second one.
        let first = sampler.sample();
        self.publish(&first);

        let mut last = Instant::now();
        while self.running.load(Ordering::SeqCst) {
            thread::sleep(TICK);
            if last.elapsed() < INTERVAL {
                continue;
            }

            last = Instant::now();
            let sample = sampler.sample();
            self.publish(&sample);
        }
    }

    /// Stores one reading and posts it to every bar.
    fn publish(&self, sample: &Sample) {
        let cpu = if sample.cpu.is_finite() {
            sample.cpu.clamp(0.0, 100.0).round() as u32
        } else {
            UNKNOWN
        };
        let ram = if sample.ram_total_gb > 0.0 {
            sample.ram_percent().round() as u32
        } else {
            UNKNOWN
        };
        let ram_mb = (sample.ram_used_gb * 1024.0)
            .round()
            .clamp(0.0, f64::from(u32::MAX)) as u32;

        self.cpu_percent.store(cpu.min(UNKNOWN), Ordering::Relaxed);
        self.ram_percent.store(ram.min(UNKNOWN), Ordering::Relaxed);
        self.ram_mb.store(ram_mb, Ordering::Relaxed);

        let targets: Vec<isize> = {
            let targets = self.targets.lock().unwrap_or_else(|lock| lock.into_inner());
            targets.clone()
        };

        for target in targets {
            crate::window::post_metrics(target, cpu, ram);
        }
    }
}

impl Drop for Metrics {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// A percentage as the bar shows it: `None` when the value is the unknown marker.
fn reading(value: u32) -> Option<u32> {
    (value <= 100).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unknown_marker_is_one_past_the_last_percentage() {
        assert_eq!(UNKNOWN, 101);
        assert_eq!(reading(100), Some(100));
        assert_eq!(reading(101), None);
    }

    #[test]
    fn a_fresh_feed_reports_something_plausible() {
        let metrics = Metrics::start();
        let snapshot = metrics.snapshot();

        assert!((0.0..=100.0).contains(&snapshot.cpu), "{snapshot:?}");
        assert!(snapshot.ram_mb < 1024 * 1024, "{snapshot:?}");
        metrics.stop();
    }

    #[test]
    fn the_worker_samples_memory_without_waiting_for_a_cpu_delta() {
        let metrics = Metrics::start();

        // The first reading is published immediately; only the CPU share needs two samples.
        let deadline = Instant::now() + Duration::from_secs(2);
        while metrics.snapshot().ram_mb == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(metrics.snapshot().ram_mb > 0, "a machine has memory in use");
        metrics.stop();
    }

    #[test]
    fn stopping_is_prompt_and_idempotent() {
        let metrics = Metrics::start();
        let started = Instant::now();

        metrics.stop();
        metrics.stop();

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "stopping took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_target_list_replaces_rather_than_appends() {
        let metrics = Metrics::start();

        metrics.set_targets(&[1, 2]);
        assert_eq!(*metrics.targets.lock().expect("lock"), vec![1, 2]);
        metrics.set_targets(&[3]);
        assert_eq!(*metrics.targets.lock().expect("lock"), vec![3]);

        metrics.stop();
    }
}
