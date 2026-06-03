#!/usr/bin/env bash
# Records src/interpreter/library/groovy_methods.txt: the names of the Groovy methods of some
# Java classes, with the Groovy of Nextflow 26.04.6. Run it from this folder.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
jar="$HOME/.nextflow/framework/26.04.6/nextflow-26.04.6-one.jar"
java -cp "$jar" groovy.ui.GroovyMain methods.groovy > ../../../../src/interpreter/library/groovy_methods.txt
