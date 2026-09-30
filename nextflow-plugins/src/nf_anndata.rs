//! nf-anndata 0.5: `anndata(path)` opens an AnnData file, which is HDF5. Reading the data is not
//! supported yet. A file that is not HDF5, like the empty `.h5ad` that a stub makes, cannot be
//! opened, as with the plugin.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::{PathValue, Value};

use crate::Context;

/// The signature of an HDF5 file, which is at the start or after a user block of 512, 1024,
/// 2048 or more bytes.
const SIGNATURE: &[u8] = b"\x89HDF\r\n\x1a\n";

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    let [target] = arguments.as_slice() else {
        return Err(unsupported(name));
    };
    if name != "anndata" {
        return Err(unsupported(name));
    }
    let path = match target {
        Value::Path(path) => (**path).clone(),
        other => PathValue::parse(&other.to_string(), &context.host.launch_dir()),
    };
    let bytes = context.host.read(&path)?;
    if !is_hdf5(&bytes) {
        let message = format!(
            "Cannot open {} as AnnData: it is not an HDF5 file",
            path.uri()
        );
        return Err(Error::Thrown {
            exception: Rc::new(Exception::new("IOException", message)),
            span: Default::default(),
        });
    }
    Err(Error::Unsupported {
        message: format!("reading the AnnData file {}", path.uri()),
        span: Default::default(),
    })
}

fn is_hdf5(bytes: &[u8]) -> bool {
    let mut offset = 0;
    loop {
        match bytes.get(offset..offset + SIGNATURE.len()) {
            Some(start) if start == SIGNATURE => return true,
            Some(_) => offset = if offset == 0 { 512 } else { offset * 2 },
            None => return false,
        }
    }
}

fn unsupported(name: &str) -> Error {
    Error::Unsupported {
        message: format!("the function `{name}` of the plugin `nf-anndata` with these arguments"),
        span: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::is_hdf5;

    #[test]
    fn finds_hdf5_signature() {
        let mut bytes = vec![0; 1024];
        bytes.extend_from_slice(super::SIGNATURE);
        assert!(is_hdf5(&bytes));
        assert!(is_hdf5(super::SIGNATURE));
        assert!(!is_hdf5(b""));
        assert!(!is_hdf5(&[0; 4096]));
    }
}
