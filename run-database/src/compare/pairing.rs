//! How the tasks of two runs pair up: step by step, first the tasks with the same fingerprint,
//! then those with the same name, then those that read and make the most files of the same
//! names, and the rest in order.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet, VecDeque};

use super::Loaded;

/// Above this many unpaired tasks in a step, the rest are paired in order, without comparing
/// their inputs, which takes time that grows with the square of their number.
const SCORED_MOST: usize = 1000;

/// The pairs of tasks, by their places in the lists of the two runs: steps in the order of
/// the first run, then the steps that only the second run has.
pub(super) fn paired(first: &[Loaded], second: &[Loaded]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut steps: Vec<&str> = Vec::new();
    let mut by_step: HashMap<&str, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (index, task) in first.iter().enumerate() {
        let entry = by_step.entry(&task.step).or_insert_with(|| {
            steps.push(&task.step);
            Default::default()
        });
        entry.0.push(index);
    }
    for (index, task) in second.iter().enumerate() {
        let entry = by_step.entry(&task.step).or_insert_with(|| {
            steps.push(&task.step);
            Default::default()
        });
        entry.1.push(index);
    }
    let mut pairs = Vec::new();
    let mut only_second = Vec::new();
    for step in steps {
        let (a, b) = &by_step[step];
        let matched = pair_step(first, second, a, b);
        let mut taken = HashSet::new();
        for (x, y) in &matched {
            taken.insert(*y);
            pairs.push((Some(*x), *y));
        }
        only_second.extend(b.iter().copied().filter(|y| !taken.contains(&Some(*y))));
    }
    pairs.extend(only_second.into_iter().map(|y| (None, Some(y))));
    pairs
}

/// Pairs the tasks of one step: each task of the first run, in order, with its partner in the
/// second run, if it has one.
fn pair_step(
    first: &[Loaded],
    second: &[Loaded],
    a: &[usize],
    b: &[usize],
) -> Vec<(usize, Option<usize>)> {
    let mut partner: HashMap<usize, usize> = HashMap::new();
    let mut free: Vec<usize> = b.to_vec();
    // The same fingerprint, then the same name.
    let keys: [fn(&Loaded) -> &str; 2] = [fingerprint_of, name_of];
    for key in keys {
        let mut waiting: HashMap<&str, VecDeque<usize>> = HashMap::new();
        for &y in &free {
            waiting.entry(key(&second[y])).or_default().push_back(y);
        }
        for &x in a {
            if partner.contains_key(&x) {
                continue;
            }
            if let Some(y) = waiting
                .get_mut(key(&first[x]))
                .and_then(VecDeque::pop_front)
            {
                partner.insert(x, y);
            }
        }
        let taken: HashSet<usize> = partner.values().copied().collect();
        free.retain(|y| !taken.contains(y));
    }
    // The most inputs and outputs of the same names, then the rest in order.
    let unpaired: Vec<usize> = a
        .iter()
        .copied()
        .filter(|x| !partner.contains_key(x))
        .collect();
    let scored = unpaired.len() <= SCORED_MOST && free.len() <= SCORED_MOST;
    for x in unpaired {
        let best = if scored {
            (free.iter().enumerate())
                .map(|(place, &y)| (overlap(&first[x], &second[y]), place))
                .filter(|(score, _)| *score > 0)
                .max_by_key(|(score, place)| (*score, std::cmp::Reverse(*place)))
                .map(|(_, place)| place)
        } else {
            None
        };
        let place = best.or((!free.is_empty()).then_some(0));
        if let Some(place) = place {
            partner.insert(x, free.remove(place));
        }
    }
    a.iter().map(|x| (*x, partner.get(x).copied())).collect()
}

fn fingerprint_of(task: &Loaded) -> &str {
    &task.fingerprint
}

fn name_of(task: &Loaded) -> &str {
    &task.side.name
}

/// How many input paths and output patterns two tasks share.
fn overlap(a: &Loaded, b: &Loaded) -> usize {
    let inputs: HashSet<&str> = (a.description.inputs.iter())
        .map(|input| input.path.as_str())
        .collect();
    let outputs: HashSet<&str> = a.description.outputs.iter().map(String::as_str).collect();
    let shared_inputs = (b.description.inputs.iter())
        .filter(|input| inputs.contains(input.path.as_str()))
        .count();
    let shared_outputs = (b.description.outputs.iter())
        .filter(|output| outputs.contains(output.as_str()))
        .count();
    shared_inputs + shared_outputs
}
