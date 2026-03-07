#!/bin/sh
# Records oracle.txt with the local Nextflow: `sh record.sh` in this directory.
NXF_OFFLINE=true nextflow config -flat -profile b,a . > oracle.txt
