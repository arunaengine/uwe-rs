# Greets the world from one task and reads the greeting back as a workflow output.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
version 1.2

task say_hello {
  input {
    String greeting
  }

  command <<<
    echo "~{greeting}, world" > greeting.txt
  >>>

  output {
    File out = "greeting.txt"
  }

  requirements {
    container: "debian:stable-slim"
    cpu: 1
    memory: "256 MiB"
  }
}

workflow hello {
  input {
    String greeting = "Hello"
  }

  call say_hello { input: greeting = greeting }

  output {
    File greeting_file = say_hello.out
    String message = read_string(say_hello.out)
  }
}
