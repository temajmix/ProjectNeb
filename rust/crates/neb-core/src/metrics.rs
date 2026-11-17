//! Machine load: how busy the CPU is and how much physical memory is in use.
//!
//! Both readings come from Win32 counters, so they describe the whole machine and cost no child
//! process: `GetSystemTimes` gives cumulative idle/kernel/user times which become a percentage once
//! two readings are compared, and `GlobalMemoryStatusEx` gives total and available physical memory.
//!
//! The first CPU reading of a [`Sampler`] is `0.0` by definition: a percentage needs a delta, and
//! inventing one from the boot-time totals would report the average since boot instead of now.
//! Nothing in this module panics: an API failure leaves the affected field at zero and is reported
//! through [`Sampler::last_error`].

use std::mem::size_of;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetSystemTimes};

/// Bytes in a gibibyte, the unit the dashboard shows.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// One reading of the machine's load.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    /// CPU busy share in the interval since the previous sample, `0.0..=100.0`.
    pub cpu: f32,
    /// Physical memory in use, in GiB.
    pub ram_used_gb: f64,
    /// Physical memory installed, in GiB.
    pub ram_total_gb: f64,
}

impl Sample {
    /// Memory in use as a percentage of installed memory, `0.0..=100.0`.
    pub fn ram_percent(&self) -> f32 {
        if self.ram_total_gb <= 0.0 {
            return 0.0;
        }

        clamp_percent((self.ram_used_gb / self.ram_total_gb * 100.0) as f32)
    }
}

/// Working set of this process in bytes, or `None` when the API refuses to say.
///
/// This is the number a memory budget is about: pages the process currently has resident.
pub fn working_set_bytes() -> Option<usize> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };

    // SAFETY: the pseudo handle of this process needs no closing, and `counters` is a live
    // structure whose size is passed in `cb`, as the API requires.
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) }.ok()?;

    Some(counters.WorkingSetSize)
}

/// Reads the machine's load, remembering the previous CPU totals between calls.
#[derive(Debug, Default)]
pub struct Sampler {
    previous: Option<CpuTimes>,
    last_error: Option<String>,
}

impl Sampler {
    /// A sampler with no previous reading, so the first [`sample`](Self::sample) reports CPU `0.0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Why the last sample could not read something, if it could not.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Takes a reading.
    ///
    /// An API that fails leaves its field at zero and records the reason in
    /// [`last_error`](Self::last_error); it never panics and never returns a wild number.
    pub fn sample(&mut self) -> Sample {
        self.last_error = None;

        let cpu = match read_cpu_times() {
            Ok(current) => {
                let previous = self.previous.replace(current);
                previous.map_or(0.0, |previous| busy_percent(previous, current))
            }
            Err(error) => {
                // Without a fresh reading a delta would mix intervals, so the next sample starts
                // over and reports 0.0 again.
                self.previous = None;
                self.last_error = Some(format!("cannot read system times: {error}"));
                0.0
            }
        };

        let (ram_used_gb, ram_total_gb) = match read_memory() {
            Ok((total, available)) => (
                total.saturating_sub(available) as f64 / GIB,
                total as f64 / GIB,
            ),
            Err(error) => {
                self.last_error = Some(format!("cannot read physical memory: {error}"));
                (0.0, 0.0)
            }
        };

        Sample {
            cpu,
            ram_used_gb,
            ram_total_gb,
        }
    }
}

/// Cumulative CPU times, in the 100 ns units `GetSystemTimes` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    /// Time the CPU spent idle.
    idle: u64,
    /// Idle plus kernel plus user: the denominator of the busy share.
    total: u64,
}

/// The busy share between two readings, clamped to a percentage.
fn busy_percent(previous: CpuTimes, current: CpuTimes) -> f32 {
    let idle = current.idle.saturating_sub(previous.idle);
    let total = current.total.saturating_sub(previous.total);

    if total == 0 {
        return 0.0;
    }

    clamp_percent((total.saturating_sub(idle) as f64 * 100.0 / total as f64) as f32)
}

/// Clamps a percentage into `0.0..=100.0`, mapping a non-finite value to zero.
fn clamp_percent(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Reads the cumulative CPU times.
fn read_cpu_times() -> Result<CpuTimes, windows::core::Error> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: all three arguments point at live `FILETIME` values that outlive the call.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }?;

    Ok(CpuTimes {
        idle: to_u64(idle),
        // Kernel time already contains idle time, as the API documents.
        total: to_u64(kernel).saturating_add(to_u64(user)),
    })
}

/// Reads total and available physical memory in bytes.
fn read_memory() -> Result<(u64, u64), windows::core::Error> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };

    // SAFETY: `status` is a live structure of the size declared in its `dwLength` field.
    unsafe { GlobalMemoryStatusEx(&mut status) }?;

    Ok((status.ullTotalPhys, status.ullAvailPhys))
}

/// Joins the two halves of a `FILETIME` into one 64-bit count.
fn to_u64(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn the_first_sample_reports_no_cpu_until_there_is_a_delta() {
        let mut sampler = Sampler::new();

        let sample = sampler.sample();

        assert_eq!(sample.cpu, 0.0);
        assert!(sample.ram_total_gb > 0.0, "{sample:?}");
        assert!(sample.ram_used_gb >= 0.0, "{sample:?}");
        assert!(
            sample.ram_used_gb <= sample.ram_total_gb,
            "used memory cannot exceed the total: {sample:?}"
        );
        assert_eq!(sampler.last_error(), None);
    }

    #[test]
    fn a_second_sample_is_a_percentage_of_the_interval() {
        let mut sampler = Sampler::new();
        let _ = sampler.sample();

        std::thread::sleep(Duration::from_millis(120));
        let sample = sampler.sample();

        assert!(
            (0.0..=100.0).contains(&sample.cpu),
            "cpu must be a percentage, got {}",
            sample.cpu
        );
        assert!(sample.ram_total_gb > 0.0);
    }

    #[test]
    fn repeated_samples_stay_in_range() {
        let mut sampler = Sampler::new();

        for _ in 0..5 {
            let sample = sampler.sample();
            assert!((0.0..=100.0).contains(&sample.cpu), "{sample:?}");
            assert!((0.0..=100.0).contains(&sample.ram_percent()), "{sample:?}");
        }
    }

    #[test]
    fn the_ram_share_is_a_percentage_of_the_total() {
        let sample = Sample {
            cpu: 0.0,
            ram_used_gb: 4.0,
            ram_total_gb: 16.0,
        };

        assert!((sample.ram_percent() - 25.0).abs() < f32::EPSILON);
        assert_eq!(
            Sample {
                ram_total_gb: 0.0,
                ..sample
            }
            .ram_percent(),
            0.0
        );
    }

    #[test]
    fn a_busy_interval_is_the_share_that_was_not_idle() {
        let previous = CpuTimes {
            idle: 1_000,
            total: 4_000,
        };

        // 500 of the 1_000 ticks were idle: the interval was 50% busy.
        let current = CpuTimes {
            idle: 1_500,
            total: 5_000,
        };
        assert!((busy_percent(previous, current) - 50.0).abs() < f32::EPSILON);

        // A fully idle interval.
        assert_eq!(
            busy_percent(
                previous,
                CpuTimes {
                    idle: 2_000,
                    total: 5_000
                }
            ),
            0.0
        );

        // A fully busy interval.
        assert_eq!(
            busy_percent(
                previous,
                CpuTimes {
                    idle: 1_000,
                    total: 5_000
                }
            ),
            100.0
        );
    }

    #[test]
    fn a_broken_interval_cannot_produce_a_wild_number() {
        let previous = CpuTimes {
            idle: 10,
            total: 10,
        };

        // No elapsed time at all.
        assert_eq!(busy_percent(previous, previous), 0.0);

        // Counters that went backwards (a 32-bit wrap or a reset machine) clamp instead of
        // producing a negative or huge percentage.
        let rewound = CpuTimes { idle: 0, total: 0 };
        assert_eq!(busy_percent(previous, rewound), 0.0);

        // Idle larger than the total is nonsense but still yields a sane number.
        let impossible = CpuTimes {
            idle: 9_999,
            total: 5_000,
        };
        assert_eq!(busy_percent(previous, impossible), 0.0);
    }

    #[test]
    fn a_non_finite_percentage_reads_as_zero() {
        assert_eq!(clamp_percent(f32::NAN), 0.0);
        assert_eq!(clamp_percent(f32::INFINITY), 0.0);
        assert_eq!(clamp_percent(-1.0), 0.0);
        assert_eq!(clamp_percent(150.0), 100.0);
    }

    #[test]
    fn the_process_has_a_measurable_working_set() {
        let bytes = working_set_bytes().expect("this process must report its working set");

        assert!(bytes > 0, "working set must be non-zero");
    }

    #[test]
    fn a_busy_machine_reports_a_non_zero_cpu() {
        let mut sampler = Sampler::new();
        let _warm_up = sampler.sample();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hogs: Vec<_> = (0..busy_threads())
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();

        std::thread::sleep(Duration::from_millis(150));
        let sample = sampler.sample();

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for hog in hogs {
            hog.join().expect("a hog must not panic");
        }

        assert!(
            sample.cpu > 0.0,
            "a machine with busy cores must not read 0.0%: {sample:?}"
        );
    }

    /// How many cores to keep busy, leaving one for the sampler itself.
    fn busy_threads() -> usize {
        std::thread::available_parallelism().map_or(1, |count| count.get().saturating_sub(1).max(1))
    }

    #[test]
    fn a_sampler_is_reusable_and_keeps_its_error_free_state() {
        let mut sampler = Sampler::new();
        let started = Instant::now();

        sampler.sample();
        sampler.sample();

        assert_eq!(sampler.last_error(), None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "sampling must not block"
        );
    }
}
