//! ```cargo
//! [dependencies]
//! ```
json_typegen::json_typegen!("Snakemake", r###"{"input": {"positional": ["in.txt"], "named": "in2.txt"}, "output": {"positional": ["out.rs"]}, "params": {"positional": [], "null_param": null, "logical_param": true, "integer_param": 123, "double_param": 123.0, "inf_param": Infinity, "character_param": "abc 'q' \"d\" \n", "vector_param": [1, 2, 3], "list_param": [true, 123, "abc"], "dict_param": {"a": 1, "b": [1.5, 2]}, "empty": []}, "wildcards": {"positional": [], "ext": "rs"}, "threads": 2, "resources": {"positional": [], "tmpdir": "/tmp", "mem_mb": 1024, "mem": "1.02 GB", "mem_mib": 977, "disk_mb": 1000, "disk": "1 GB", "disk_mib": 954, "runtime": 10}, "log": {"positional": ["log.rs"]}, "config": {"positional": [], "test": true, "testint": 123, "testfloat": 7.65432, "foo' bar": "let's go", "nested": {"a": [1, 2], "b": "x"}}, "rulename": "plain", "bench_iteration": null, "scriptdir": "{folder}/scripts"}"###, {
    "/bench_iteration": {
       "use_type": "Option<usize>"
    },
    "/input/positional": {
        "use_type": "Vec<String>"
    },
    "/output/positional": {
        "use_type": "Vec<String>"
    },
    "/log/positional": {
        "use_type": "Vec<String>"
    },
    "/wildcards/positional": {
        "use_type": "Vec<String>"
    },
});

pub struct Iter<'a, T>(std::slice::Iter<'a, T>);
impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}
macro_rules! impl_iter {
    ($($s:ty),+) => {
        $(
            impl IntoIterator for $s {
                type Item = String;
                type IntoIter = std::vec::IntoIter<Self::Item>;

                fn into_iter(self) -> Self::IntoIter {
                    self.positional.into_iter()
                }
            }

            impl<'a> IntoIterator for &'a $s {
                type Item = &'a String;
                type IntoIter = Iter<'a, String>;

                fn into_iter(self) -> Self::IntoIter {
                    Iter(self.positional.as_slice().into_iter())
                }
            }
        )+
    };
}

macro_rules! impl_index {
    ($($s:ty),+) => {
        $(
        impl std::ops::Index<usize> for $s {
            type Output = String;

            fn index(&self, index: usize) -> &Self::Output {
                &self.positional[index]
            }
        }
        )+
    }
}


impl_iter!(Input, Output, Wildcards, Log);
impl_index!(Input, Output, Wildcards, Log);

impl Snakemake {
    #[allow(dead_code)]
    fn redirect_stderr<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> anyhow::Result<gag::Redirect<std::fs::File>> {
        let log = std::fs::OpenOptions::new()
            .truncate(true)
            .read(true)
            .create(true)
            .write(true)
            .open(path)?;
        Ok(gag::Redirect::stderr(log)?)
    }

    #[allow(dead_code)]
    fn redirect_stdout<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> anyhow::Result<gag::Redirect<std::fs::File>> {
        let log = std::fs::OpenOptions::new()
            .truncate(true)
            .read(true)
            .create(true)
            .write(true)
            .open(path)?;
        Ok(gag::Redirect::stdout(log)?)
    }
}

lazy_static::lazy_static! {
    // https://github.com/rust-lang-nursery/lazy-static.rs/issues/153
    #[allow(non_upper_case_globals)]
    static ref snakemake: Snakemake = {
        let s: Snakemake = serde_json::from_str(r###"{"input": {"positional": ["in.txt"], "named": "in2.txt"}, "output": {"positional": ["out.rs"]}, "params": {"positional": [], "null_param": null, "logical_param": true, "integer_param": 123, "double_param": 123.0, "inf_param": Infinity, "character_param": "abc 'q' \"d\" \n", "vector_param": [1, 2, 3], "list_param": [true, 123, "abc"], "dict_param": {"a": 1, "b": [1.5, 2]}, "empty": []}, "wildcards": {"positional": [], "ext": "rs"}, "threads": 2, "resources": {"positional": [], "tmpdir": "/tmp", "mem_mb": 1024, "mem": "1.02 GB", "mem_mib": 977, "disk_mb": 1000, "disk": "1 GB", "disk_mib": 954, "runtime": 10}, "log": {"positional": ["log.rs"]}, "config": {"positional": [], "test": true, "testint": 123, "testfloat": 7.65432, "foo' bar": "let's go", "nested": {"a": [1, 2], "b": "x"}}, "rulename": "plain", "bench_iteration": null, "scriptdir": "{folder}/scripts"}"###).expect("Failed parsing snakemake JSON");
        s
    };
}
// TODO include addendum, if any {preamble_addendum}
fn main() {}
