//! Checks that the split and count operators read a large gzip file a part at a time.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

/// The system allocator, counting the bytes in use and the most in use at once. This file
/// holds one test, so that no other test of the binary allocates at the same time.
struct Counting;

static USED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counting = Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        USED.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            grow(size);
            USED.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        moved
    }
}

fn grow(size: usize) {
    let used = USED.fetch_add(size, Ordering::SeqCst) + size;
    PEAK.fetch_max(used, Ordering::SeqCst);
}

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// Records in one gzip member, and members in the file: 64 MiB of FASTQ once decompressed.
const RECORDS: usize = 16 * 1024;
const MEMBERS: usize = 64;

#[test]
fn splits_large_gzip() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let reads = directory.path().join("reads.fq.gz");
    let record = format!("@read\n{}\n+\n{}\n", "A".repeat(27), "I".repeat(27));
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(record.repeat(RECORDS).as_bytes())
        .expect("compresses");
    let member = encoder.finish().expect("compresses");
    let mut file = std::fs::File::create(&reads).expect("creates the file");
    for _ in 0..MEMBERS {
        file.write_all(&member).expect("writes the file");
    }
    drop(file);
    assert_eq!(record.len() * RECORDS * MEMBERS, 64 << 20);

    let source = format!(
        "workflow {{\nchannel.fromPath('{0}').splitFastq(by: 1024).count().view()\n\
         channel.fromPath('{0}').countFastq().view()\n}}",
        reads.display()
    );
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut output = Output::default();
    let before = USED.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    nextflow_runtime::run(&module, &mut output).expect("runs");
    let peak = PEAK.load(Ordering::SeqCst) - before;
    let total = RECORDS * MEMBERS;
    let mut printed: Vec<&str> = output.0.lines().collect();
    printed.sort();
    assert_eq!(printed, [(total / 1024).to_string(), total.to_string()]);
    assert!(peak < 16 << 20, "splitting held {peak} bytes at once");
}
