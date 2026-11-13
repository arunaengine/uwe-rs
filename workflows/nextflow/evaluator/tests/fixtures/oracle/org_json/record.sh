#!/bin/sh
# Records org.json and everit Groovy results with Nextflow 26.04.6 and nf-validation 1.1.3.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
lib="$HOME/.nextflow/plugins/nf-validation-1.1.3/lib"
classes="$HOME/.nextflow/framework/26.04.6/nextflow-26.04.6-one.jar:$lib/json-20220320.jar"
classes="$classes:$lib/org.everit.json.schema-1.14.1.jar:$lib/handy-uri-templates-2.1.8.jar"
classes="$classes:$lib/re2j-1.6.jar:$lib/commons-validator-1.7.jar"
for script in "$(dirname "$0")"/*.groovy; do
	java -cp "$classes" groovy.ui.GroovyMain "$script"
done
