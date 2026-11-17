//! `neb-cli metrics` (CPU and memory) and the hidden `neb-cli mem` harness.
//!
//! The first CPU reading a process can take has no interval behind it and is always 0.0, so the
//! command takes one reading and discards it; every reported sample compares two real readings.
//! `mem` exists because the CLI's own footprint is a budget item: it prints its own working set from
//! `GetProcessMemoryInfo`, which is the number the 8 MB limit is about, measured by the process
//! itself rather than by an external tool that might count differently.

use std::time::Duration;

use neb_core::metrics::{Sample, Sampler, working_set_bytes};
use serde_json::{Value, json};

use super::{CliError, write_json, write_line};
use crate::args::Options;

/// Runs `neb-cli metrics`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    if let Some(argument) = options.positional.first() {
        return Err(CliError::Usage(format!(
            "metrics takes no arguments, got '{argument}'"
        )));
    }

    let mut sampler = Sampler::new();
    let mut samples = Vec::with_capacity(options.count as usize);

    // The first CPU reading of a process has no interval behind it, so it is always 0.0. Take it and
    // throw it away: every line this command prints is a measurement, not a placeholder. The cost is
    // one extra interval before the first reported sample.
    let _ = sampler.sample();

    for _ in 0..options.count {
        std::thread::sleep(Duration::from_millis(options.interval_ms));

        let sample = sampler.sample();

        if !options.json {
            write_line(&format!(
                "cpu: {:5.1}%  ram: {:6.2} / {:6.2} GB ({:5.1}%)",
                sample.cpu,
                sample.ram_used_gb,
                sample.ram_total_gb,
                sample.ram_percent()
            ))?;
        }

        samples.push(sample);
    }

    if let Some(error) = sampler.last_error() {
        return Err(CliError::Failed(error.to_owned()));
    }

    if options.json {
        let values: Vec<Value> = samples.iter().map(sample_json).collect();
        write_json(&Value::Array(values))?;
    }

    Ok(())
}

/// Runs `neb-cli mem`: this process's working set.
pub fn run_mem(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    if let Some(argument) = options.positional.first() {
        return Err(CliError::Usage(format!(
            "mem takes no arguments, got '{argument}'"
        )));
    }

    let bytes = working_set_bytes()
        .ok_or_else(|| CliError::Failed("cannot read this process's working set".to_owned()))?;
    let megabytes = two_decimals(bytes as f64 / (1024.0 * 1024.0));

    if options.json {
        return write_json(&json!({
            "workingSetBytes": bytes,
            "workingSetMb": megabytes,
        }));
    }

    write_line(&format!("working set: {megabytes:.2} MB ({bytes} bytes)"))
}

/// One sample as the JSON document `--json` prints.
fn sample_json(sample: &Sample) -> Value {
    json!({
        "cpu": one_decimal(f64::from(sample.cpu)),
        "ramUsedGb": two_decimals(sample.ram_used_gb),
        "ramTotalGb": two_decimals(sample.ram_total_gb),
        "ramPercent": one_decimal(f64::from(sample.ram_percent())),
    })
}

/// Rounds to one decimal place, so the output does not pretend to more precision than it has.
fn one_decimal(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Rounds to two decimal places.
fn two_decimals(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_renders_the_documented_keys() {
        let value = sample_json(&Sample {
            cpu: 12.345,
            ram_used_gb: 7.876,
            ram_total_gb: 15.9,
        });

        assert_eq!(value["cpu"], json!(12.3));
        assert_eq!(value["ramUsedGb"], json!(7.88));
        assert_eq!(value["ramTotalGb"], json!(15.9));
        assert_eq!(value["ramPercent"], json!(49.5));
    }

    #[test]
    fn rounding_never_adds_digits() {
        assert_eq!(one_decimal(0.04), 0.0);
        assert_eq!(one_decimal(0.05), 0.1);
        assert_eq!(two_decimals(1.005), 1.0);
        assert_eq!(two_decimals(1.006), 1.01);
    }

    #[test]
    fn a_zero_sample_renders_zeros_rather_than_nulls() {
        let value = sample_json(&Sample::default());

        assert_eq!(value["cpu"], json!(0.0));
        assert_eq!(value["ramPercent"], json!(0.0));
        assert!(value["ramTotalGb"].is_number());
    }

    #[test]
    fn operands_are_refused() {
        assert!(matches!(
            run(&["extra".to_owned()]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            run_mem(&["extra".to_owned()]),
            Err(CliError::Usage(_))
        ));
    }
}
