//! Right-sized resources: the CPUs and memory that a task asks for, lowered to what the tasks
//! of its step used in earlier runs, with a margin, and never above what the workflow asked.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Placement;

/// One mebibyte.
const MIB: u64 = 1024 * 1024;

/// The least memory that a right-sized task gets, so that a shell and small tools always fit.
pub const LEAST_MEMORY: u64 = 64 * MIB;

/// What one task of a step used in an earlier run, as the cgroup of its container counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    /// The bytes of the task's input files, when known.
    pub input_bytes: Option<u64>,
    /// The peak of its memory in bytes, page cache included.
    pub memory_peak: Option<u64>,
    /// Its CPU time in microseconds.
    pub cpu_usec: Option<u64>,
    /// How long its tool ran, in milliseconds.
    pub milliseconds: u64,
    /// The milliseconds that the executor spent on the task besides its tool: from the
    /// hand-over of the task to the result, less the time of the tool. It holds the start of
    /// the container, staging and storing the outputs, and any wait for a free CPU.
    pub overhead: Option<u64>,
}

/// Whether the tasks of a step are short: their tools ran, on average, for less time than the
/// executor spent on the least of them besides the tool. Such tasks save time when several run
/// one after another in one container. A step without such measures is not short.
pub fn short(measured: &[Measured]) -> bool {
    let least = measured.iter().filter_map(|task| task.overhead).min();
    let Some(least) = least else {
        return false;
    };
    let tool = measured.iter().map(|task| task.milliseconds).sum::<u64>() / measured.len() as u64;
    tool < least
}

/// The placement of a task with the CPUs and memory that the tasks of its step used in earlier
/// runs (`measured`), where that is less than it asks for:
///
/// - Memory: the peak of the earlier task with the smallest input that is at least as large as
///   this one's (`input_bytes`). When this input is larger than all earlier ones, the peak of
///   the largest grows in step with the input, and it is never less than any earlier peak.
///   Without sizes, the highest peak. A quarter more goes on top, and at least
///   [`LEAST_MEMORY`].
/// - CPUs: the most CPUs that an earlier task kept busy on average, a tenth more, rounded up.
///
/// Neither ever goes above what the task asks for, which is the most that its workflow allows.
/// A task that asks for no memory keeps none: a limit could stop a task that its workflow
/// never limited.
pub fn right_sized(
    asked: &Placement,
    input_bytes: Option<u64>,
    measured: &[Measured],
) -> Placement {
    let mut placement = asked.clone();
    if let (Some(asked), Some(peak)) = (asked.memory_bytes, peak(input_bytes, measured)) {
        let memory = (peak.saturating_add(peak / 4)).max(LEAST_MEMORY);
        placement.memory_bytes = Some(memory.div_ceil(MIB).saturating_mul(MIB).min(asked));
    }
    if let Some(busy) = busy_cpus(measured) {
        let cpus = (busy * 1.1).ceil().max(1.0) as u32;
        placement.cpus = cpus.min(asked.cpus.max(1));
    }
    placement
}

/// The expected peak of memory of a task with `input_bytes`, from the earlier tasks of its step.
fn peak(input_bytes: Option<u64>, measured: &[Measured]) -> Option<u64> {
    let peaks = measured
        .iter()
        .filter_map(|task| Some((task.input_bytes, task.memory_peak?)));
    let highest = peaks.clone().map(|(_, peak)| peak).max()?;
    let Some(input) = input_bytes else {
        return Some(highest);
    };
    let sized: Vec<(u64, u64)> = peaks
        .filter_map(|(bytes, peak)| Some((bytes?, peak)))
        .collect();
    // The earlier tasks with at least this much input: the one with the least input tells.
    let larger = (sized.iter())
        .filter(|(bytes, _)| *bytes >= input)
        .min_by_key(|(bytes, peak)| (*bytes, std::cmp::Reverse(*peak)));
    if let Some((_, peak)) = larger {
        return Some(*peak);
    }
    // More input than ever before: the peak of the largest, grown in step.
    let Some(&(bytes, peak)) = sized.iter().max_by_key(|(bytes, peak)| (*bytes, *peak)) else {
        return Some(highest);
    };
    let grown = (u128::from(peak) * u128::from(input) / u128::from(bytes.max(1))) as u64;
    Some(grown.max(highest))
}

/// The most CPUs that an earlier task of the step kept busy, on average over its time.
fn busy_cpus(measured: &[Measured]) -> Option<f64> {
    (measured.iter())
        .filter(|task| task.milliseconds > 0)
        .filter_map(|task| Some(task.cpu_usec? as f64 / (task.milliseconds as f64 * 1000.0)))
        .reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    fn asked(cpus: u32, memory: Option<u64>) -> Placement {
        Placement {
            cpus,
            memory_bytes: memory,
            ..Placement::default()
        }
    }

    fn task(input: Option<u64>, peak: u64, cpu_seconds: u64, seconds: u64) -> Measured {
        Measured {
            input_bytes: input,
            memory_peak: Some(peak),
            cpu_usec: Some(cpu_seconds * 1_000_000),
            milliseconds: seconds * 1000,
            overhead: None,
        }
    }

    #[test]
    fn short_tasks_ran_for_less_time_than_the_executor_spent_besides() {
        let ran = |milliseconds, overhead| Measured {
            milliseconds,
            overhead,
            ..Measured::default()
        };
        assert!(short(&[ran(100, Some(800)), ran(300, Some(2000))]));
        assert!(!short(&[ran(900, Some(800)), ran(900, Some(2000))]));
        assert!(!short(&[ran(100, None)]));
        assert!(!short(&[]));
    }

    #[test]
    fn nothing_measured_keeps_what_the_task_asks_for() {
        let placement = asked(4, Some(6 * GIB));
        assert_eq!(right_sized(&placement, Some(10), &[]), placement);
    }

    #[test]
    fn memory_and_cpus_come_down_to_what_was_used_with_a_margin() {
        // 400 MiB at most, 1.5 CPUs busy on average.
        let measured = [task(None, 400 * MIB, 15, 10), task(None, 300 * MIB, 5, 10)];
        let sized = right_sized(&asked(4, Some(6 * GIB)), None, &measured);
        assert_eq!(sized.memory_bytes, Some(500 * MIB));
        assert_eq!(sized.cpus, 2);
    }

    #[test]
    fn never_more_than_the_task_asks_for() {
        let measured = [task(None, 4 * GIB, 80, 10)];
        let sized = right_sized(&asked(2, Some(GIB)), None, &measured);
        assert_eq!((sized.cpus, sized.memory_bytes), (2, Some(GIB)));
        // A task that names no memory gets no limit.
        let sized = right_sized(&asked(2, None), None, &measured);
        assert_eq!(sized.memory_bytes, None);
    }

    #[test]
    fn small_peaks_get_the_least_memory() {
        let sized = right_sized(&asked(1, Some(GIB)), None, &[task(None, MIB, 1, 10)]);
        assert_eq!(sized.memory_bytes, Some(LEAST_MEMORY));
    }

    #[test]
    fn memory_follows_the_size_of_the_input() {
        let measured = [
            task(Some(100 * MIB), 200 * MIB, 1, 1),
            task(Some(1000 * MIB), 800 * MIB, 1, 1),
        ];
        let memory = |input| right_sized(&asked(1, Some(64 * GIB)), Some(input), &measured);
        // A small input: the peak of the smallest earlier input that is at least as large.
        assert_eq!(memory(50 * MIB).memory_bytes, Some(250 * MIB));
        assert_eq!(memory(500 * MIB).memory_bytes, Some(1000 * MIB));
        // Twice the largest earlier input: twice its peak.
        assert_eq!(memory(2000 * MIB).memory_bytes, Some(2000 * MIB));
    }
}
