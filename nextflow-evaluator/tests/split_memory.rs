//! Checks that the count methods of paths read a large gzip file a part at a time.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nextflow_evaluator::interpreter::{Host, Interpreter};
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

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

/// Records in one gzip member, and members in the file: 256 MiB of FASTQ once decompressed.
const RECORDS: usize = 16 * 1024;
const MEMBERS: usize = 256;

#[test]
fn counts_large_gzip() {
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
    assert_eq!(record.len() * RECORDS * MEMBERS, 256 << 20);

    let source = format!(
        "def f() {{\ndef reads = file('{}')\n[reads.countFastq(), reads.countLines()]\n}}",
        reads.display()
    );
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Quiet;
    let mut interpreter = Interpreter::new(&mut host);
    let before = USED.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let counts = interpreter
        .call_function(&module, "f", Vec::new())
        .expect("counts the records");
    let peak = PEAK.load(Ordering::SeqCst) - before;
    let total = RECORDS * MEMBERS;
    assert_eq!(counts.to_string(), format!("[{total}, {}]", total * 4));
    assert!(peak < 16 << 20, "counting held {peak} bytes at once");
}
