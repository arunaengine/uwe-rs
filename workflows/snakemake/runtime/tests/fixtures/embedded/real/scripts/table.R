snakemake@source("common.R")
lines <- readLines(snakemake@input[["data"]])
counts <- scaled(nchar(lines), snakemake@params[["factor"]])
out <- c(
    paste("rule", snakemake@rule, "sample", snakemake@wildcards[["sample"]], "threads", snakemake@threads),
    paste("config", snakemake@config[["greeting"]], paste(snakemake@config[["samples"]], collapse = ",")),
    paste("flags", paste(snakemake@params[["flags"]], collapse = ",")),
    paste("scaled", paste(counts, collapse = ","))
)
writeLines(out, snakemake@output[["table"]])
