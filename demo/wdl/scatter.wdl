# Greets several names in parallel tasks and collects the greetings in one file.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
version 1.2

task greet {
  input {
    String name
  }

  command <<<
    echo "Hello, ~{name}"
  >>>

  output {
    String line = read_string(stdout())
  }

  requirements {
    container: "debian:stable-slim"
  }
}

task collect {
  input {
    Array[String] lines
  }

  command <<<
    printf '%s\n' ~{sep(" ", squote(lines))} > all.txt
  >>>

  output {
    File all = "all.txt"
    Int count = length(read_lines("all.txt"))
  }

  requirements {
    container: "debian:stable-slim"
  }
}

workflow scatter_names {
  input {
    Array[String] names = ["Ada", "Grace", "Linus"]
  }

  scatter (name in names) {
    call greet { input: name = name }
  }

  call collect { input: lines = greet.line }

  output {
    File all = collect.all
    Int count = collect.count
  }
}
