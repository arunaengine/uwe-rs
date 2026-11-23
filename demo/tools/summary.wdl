# Counts the lines and words of a text into a short report, as a WDL task.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
version 1.2

task summarize {
  input {
    File text
  }

  command <<<
    echo "lines $(wc -l < ~{text})" > summary.txt
    echo "words $(wc -w < ~{text})" >> summary.txt
  >>>

  output {
    File report = "summary.txt"
  }

  requirements {
    container: "debian:stable-slim"
  }
}
