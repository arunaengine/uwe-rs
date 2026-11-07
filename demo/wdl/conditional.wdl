# Shouts a text in a task only when asked to, and else passes it on unchanged.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
version 1.2

task shout {
  input {
    String text
  }

  command <<<
    echo "~{text}" | tr '[:lower:]' '[:upper:]'
  >>>

  output {
    String loud = read_string(stdout())
  }

  requirements {
    container: "debian:stable-slim"
  }
}

workflow conditional {
  input {
    String text = "hello"
    Boolean loud = true
  }

  if (loud) {
    call shout { input: text = text }
  }

  if (!loud) {
    String quiet = text
  }

  output {
    String result = select_first([shout.loud, quiet])
  }
}
