
######## snakemake preamble start (automatically inserted, do not edit) ########
struct Snakemake
    input::Dict
    output::Dict
    params::Dict
    wildcards::Dict
    threads::Int64
    log::Dict
    resources::Dict
    config::Dict
    rule::String
    bench_iteration
    scriptdir::String
    #source::Any
end
snakemake = Snakemake(
    Dict(1 => "in.txt", 2 => "in2.txt", "named" => "in2.txt"), #input::Dict
    Dict(1 => "out.jl", ), #output::Dict
    Dict(1 => nothing, 2 => true, 3 => 123, 4 => 123.0, 5 => inf, 6 => "abc 'q' \"d\" \n", 7 => [1, 2, 3], 8 => [true, 123, "abc"], 9 => Dict("a" => 1, "b" => [1.5, 2]), 10 => [], "null_param" => nothing, "logical_param" => true, "integer_param" => 123, "double_param" => 123.0, "inf_param" => inf, "character_param" => "abc 'q' \"d\" \n", "vector_param" => [1, 2, 3], "list_param" => [true, 123, "abc"], "dict_param" => Dict("a" => 1, "b" => [1.5, 2]), "empty" => []), #params::Dict
    Dict(1 => "jl", "ext" => "jl"), #wildcards::Dict
    2, #threads::Int64
    Dict(1 => "log.jl", ), #log::Dict
    Dict(1 => "tmpdir", 2 => "mem_mb", 3 => "mem", 4 => "mem_mib", 5 => "disk_mb", 6 => "disk", 7 => "disk_mib", 8 => "runtime", "tmpdir" => "/tmp", "mem_mb" => 1024, "mem" => "1.02 GB", "mem_mib" => 977, "disk_mb" => 1000, "disk" => "1 GB", "disk_mib" => 954, "runtime" => 10), #resources::Dict
    Dict("test" => true, "testint" => 123, "testfloat" => 7.65432, "foo' bar" => "let's go", "nested" => Dict("a" => [1, 2], "b" => "x")), #config::Dict
    "plain", #rule::String
    nothing, #bench_iteration::Int64
    "{folder}/scripts", #scriptdir::String
    #, #source::Any
)
######## snakemake preamble end #########
# body
