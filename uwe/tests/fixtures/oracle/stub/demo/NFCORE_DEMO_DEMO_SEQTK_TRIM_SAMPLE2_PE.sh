#!/usr/bin/env bash -e -u -o pipefail
printf "%s\n" sample2_R1.fastq.gz sample2_R2.fastq.gz | while read f;
do
    seqtk \
        trimfq \
         \
        $f \
        | gzip --no-name > SAMPLE2_PE_$(basename $f)
done
