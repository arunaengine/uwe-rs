#!/usr/bin/env bash -e -u -o pipefail
printf "%s\n" sample1_R1.fastq.gz sample1_R2.fastq.gz | while read f;
do
    seqtk \
        trimfq \
         \
        $f \
        | gzip --no-name > SAMPLE1_PE_$(basename $f)
done
