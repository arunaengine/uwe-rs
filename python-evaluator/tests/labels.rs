//! Regressions for label-sensitive table operations.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};

fn evaluate(code: &str) -> Result<String, python_evaluator::Error> {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", code)?;
    let output = host.output.borrow().clone();
    Ok(output)
}

#[test]
fn aligns_series_arithmetic() {
    let output = evaluate(
        "import pandas as pd\na = pd.Series([1, 2], index=['A', 'B'])\nb = pd.Series([10, 20], index=['B', 'A'])\nprint((a + b).tolist())\nprint((a + pd.Series([3], index=['C'])).isna().tolist())\ntry:\n    a == b\nexcept ValueError:\n    print('comparison rejected')\nprint((a == a).tolist())\nprint((pd.Series([1, 2], index=['S', 'S']) + pd.Series([3, 4], index=['S', 'S'])).tolist())\n",
    ).expect("evaluates");
    assert_eq!(
        output,
        "[21, 12]\n[True, True, True]\ncomparison rejected\n[True, True]\n[4, 6]\n"
    );
}

#[test]
fn aligns_boolean_masks() {
    let output = evaluate(
        "import pandas as pd\ns = pd.Series([1, 2], index=['A', 'B'])\nf = pd.DataFrame({'x': [1, 2]}, index=['A', 'B'])\nm = pd.Series([True, False], index=['B', 'A'])\nprint(s[m].tolist(), f[m]['x'].tolist(), f.loc[m]['x'].tolist())\nprint(s[[True, False]].tolist())\ntry:\n    f[pd.Series([True, False], index=['C', 'D'])]\nexcept Exception:\n    print('unalignable rejected')\n",
    ).expect("evaluates");
    assert_eq!(output, "[2] [2] [2]\n[1]\nunalignable rejected\n");
}

#[test]
fn aligns_missing_labels() {
    let output = evaluate(
        "import pandas as pd\nn = float('nan')\na = pd.Series([1, 2], index=[n, 1])\nb = pd.Series([20, 10], index=[1, n])\nr = a + b\nfor label, value in zip(r.index, r):\n    if label != label:\n        print(value)\nprint(r.loc[1])\nm = pd.Series([False, True], index=[1, n])\nprint(a[m].tolist())\na[pd.Series([False, False], index=[1, n])] = 99\nprint(a.tolist())\na[m] = 7\nprint(a.tolist())\ntry:\n    a[pd.Series([False, True, False], index=[1, n, n])]\nexcept Exception:\n    print('duplicate mask rejected')\ntry:\n    pd.concat([pd.DataFrame({'x': [1, 2]}, index=[n, n]), pd.DataFrame({'y': [3]}, index=[1])], axis=1)\nexcept Exception:\n    print('duplicate concat rejected')\n",
    ).expect("evaluates");
    assert_eq!(
        output,
        "11\n22\n[1]\n[1, 2]\n[7, 2]\nduplicate mask rejected\nduplicate concat rejected\n"
    );
}

#[test]
fn orders_missing_labels() {
    let output = evaluate(
        "import pandas as pd\nn = float('nan')\na = pd.Series([1, 2], index=[n, 1])\nb = pd.Series([20, 10], index=[1, n])\nr = a + b\nprint(r.index[0] == 1, r.index[1] != r.index[1], r.tolist())\n",
    ).expect("evaluates");
    assert_eq!(output, "True True [22, 11]\n");
}

#[test]
fn rejects_ambiguous_arithmetic() {
    let error = evaluate(
        "import pandas as pd\nn = float('nan')\npd.Series([1, 2], index=[n, n]) + pd.Series([3], index=[1])\n",
    ).expect_err("ambiguous alignment");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn preserves_duplicate_indexes() {
    let output = evaluate(
        "import pandas as pd\na = pd.DataFrame({'a': [1, 2]}, index=['S', 'S'])\nb = pd.DataFrame({'b': [3, 4]}, index=['S', 'S'])\nf = pd.concat([a, b], axis=1)\nprint(f.shape, f['a'].tolist(), f['b'].tolist())\ntry:\n    pd.concat([a, pd.DataFrame({'b': [3, 4]}, index=['S', 'T'])], axis=1)\nexcept Exception:\n    print('ambiguous rejected')\n",
    ).expect("evaluates");
    assert_eq!(output, "(2, 2) [1, 2] [3, 4]\nambiguous rejected\n");
}

#[test]
fn honors_label_slices() {
    let output = evaluate(
        "import pandas as pd\ns = pd.Series([1, 2, 3, 4], index=['A', 'B', 'C', 'D'])\nprint(s.loc[::-1].tolist(), s.loc[::2].tolist())\nprint(s.loc['D':'B':-2].tolist(), s.loc['B':'D':2].tolist())\nprint(s.loc['A':'D':-1].tolist())\ntry:\n    s.loc[::0]\nexcept ValueError:\n    print('zero rejected')\n",
    ).expect("evaluates");
    assert_eq!(
        output,
        "[4, 3, 2, 1] [1, 3]\n[4, 2] [2, 4]\n[]\nzero rejected\n"
    );
}

#[test]
fn combines_filter_constraints() {
    let output = evaluate(
        "import polars as pl\nf = pl.DataFrame({'sample': ['A', 'B', 'A'], 'n': [1, 2, 3]})\nprint(f.filter(sample='A')['n'].to_list())\nprint(f.filter(pl.col('n') > 1, sample='A')['n'].to_list())\nprint(f.lazy().filter(sample='B').collect()['n'].to_list())\nprint(f.lazy().filter(pl.col('n') > 1, sample='A').collect()['n'].to_list())\n",
    ).expect("evaluates");
    assert_eq!(output, "[1, 3]\n[3]\n[2]\n[3]\n");
}
