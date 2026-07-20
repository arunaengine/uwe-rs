#!/usr/bin/env bash
declare -A snakemake_input=( [0]=in.txt [named]=in2.txt [1]=in2.txt )
declare -A snakemake_output=( [0]=out.sh )
declare -A snakemake_params=( [nan_param]=nan [0]=nan [complex_param]='(1+2j)' [1]='(1+2j)' [null_param]=None [2]=None [logical_param]=True [3]=True [integer_param]=123 [4]=123 [double_param]=123.0 [5]=123.0 [inf_param]=inf [6]=inf [character_param]='abc '"'"'q'"'"' "d" 
' [7]='abc '"'"'q'"'"' "d" 
' [vector_param]='1 2 3' [8]='1 2 3' [list_param]='True 123 abc' [9]='True 123 abc' [dict_param]='a b' [10]='a b' [empty]='' [11]='' )
declare -A snakemake_wildcards=( [ext]=sh [0]=sh )
declare -A snakemake_resources=( [tmpdir]=/tmp [0]=/tmp [mem_mb]=1024 [1]=1024 [mem]='1.02 GB' [2]='1.02 GB' [mem_mib]=977 [3]=977 [disk_mb]=1000 [4]=1000 [disk]='1 GB' [5]='1 GB' [disk_mib]=954 [6]=954 [_nodes]=1 [7]=1 [runtime]=10 [8]=10 [_cores]=2 [9]=2 )
declare -A snakemake_log=( [0]=log.sh )
declare -A snakemake_config=( [test]=True [testint]=123 [testfloat]=7.65432 ['foo'"'"' bar']='let'"'"'s go' [nested]='a b' )
declare -A snakemake=( [_params_types]='' [threads]=2 [rule]=full [bench_iteration]=None [scriptdir]={folder}/scripts )
# body
