lines <- readLines(snakemake@input[[1]])
writeLines(rev(lines), snakemake@output[[1]])
