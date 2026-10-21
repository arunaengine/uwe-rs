#!/usr/bin/env bash -e -u -o pipefail
printf "%s\n" sample1_R1.fastq.gz sample2_R1.fastq.gz | while read f;
do
    seqtk \
        trimfq \
         \
        $f \
        | gzip --no-name > SAMPLE3_SE_$(basename $f)
done
