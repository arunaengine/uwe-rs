#!/bin/sh
# Records oracle.txt with local Nextflow: run `sh record.sh` in this directory.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
NXF_OFFLINE=true nextflow config -flat -profile b,a . > oracle.txt
