# The stand-in of the snakemake package that iunctus gives the code of jobs in their
# containers: the `snakemake` object of scripts, wrappers and notebooks, `shell()`, and the
# helpers that such code imports from snakemake. See
# docs/design/snakemake/embedded-code.md. It needs only the Python standard library, and runs
# with Python 3.7 or later, as the scripts of Snakemake 9.27 do.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

__version__ = "9.27.0"

from snakemake.shell import shell  # noqa: E402,F401
