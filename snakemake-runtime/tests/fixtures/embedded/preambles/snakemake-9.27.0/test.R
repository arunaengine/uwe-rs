
######## snakemake preamble start (automatically inserted, do not edit) ########
library(methods)
Snakemake <- setClass(
    "Snakemake",
    slots = c(
        input = "list",
        output = "list",
        params = "list",
        wildcards = "list",
        threads = "numeric",
        log = "list",
        resources = "list",
        config = "list",
        rule = "character",
        bench_iteration = "numeric",
        scriptdir = "character",
        source = "function"
    )
)
snakemake <- Snakemake(
    input = list('in.txt', 'in2.txt', "named" = 'in2.txt'),
    output = list('out.R'),
    params = list(NaN, 1.0+2.0i, NULL, TRUE, 123L, 123.0, Inf, 'abc \'q\' "d" \n', c(1L, 2L, 3L), list(TRUE, 123L, 'abc'), list("a" = 1L, "b" = c(1.5, 2L)), c(), "nan_param" = NaN, "complex_param" = 1.0+2.0i, "null_param" = NULL, "logical_param" = TRUE, "integer_param" = 123L, "double_param" = 123.0, "inf_param" = Inf, "character_param" = 'abc \'q\' "d" \n', "vector_param" = c(1L, 2L, 3L), "list_param" = list(TRUE, 123L, 'abc'), "dict_param" = list("a" = 1L, "b" = c(1.5, 2L)), "empty" = c()),
    wildcards = list('R', "ext" = 'R'),
    threads = 2,
    log = list('log.R'),
    resources = list('tmpdir', 'mem_mb', 'mem', 'mem_mib', 'disk_mb', 'disk', 'disk_mib', 'runtime', "tmpdir" = '/tmp', "mem_mb" = 1024L, "mem" = '1.02 GB', "mem_mib" = 977L, "disk_mb" = 1000L, "disk" = '1 GB', "disk_mib" = 954L, "runtime" = 10L),
    config = list("test" = TRUE, "testint" = 123L, "testfloat" = 7.65432, "foo' bar" = "let's go", "nested" = list("a" = c(1L, 2L), "b" = 'x')),
    rule = 'full',
    bench_iteration = as.numeric(NA),
    scriptdir = '{folder}/scripts',
    source = function(...){
        old_wd <- getwd()
        on.exit(setwd(old_wd), add = TRUE)

        is_url <- grepl("^https?://", snakemake@scriptdir)
        file <- ifelse(is_url, file.path(snakemake@scriptdir, ...), ...)
        if (!is_url) setwd(snakemake@scriptdir)
        source(file)
    }
)


######## snakemake preamble end #########
# body
