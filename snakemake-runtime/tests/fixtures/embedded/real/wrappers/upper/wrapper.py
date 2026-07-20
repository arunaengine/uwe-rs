__author__ = "iunctus"

from snakemake.shell import shell

extra = snakemake.params.get("extra", "")
log = snakemake.log_fmt_shell(stdout=False, stderr=True)
upper = "tr a-z A-Z" if extra == "--upper" else "cat"
shell("{upper} < {snakemake.input[0]} > {snakemake.output[0]} {log}")
