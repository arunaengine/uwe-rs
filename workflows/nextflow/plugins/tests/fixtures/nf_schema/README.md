<!-- Inputs and recorded outputs of the real nf-schema 2.7.3, 2.8.0 and 3.0.0 for the tests. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# nf-schema fixtures

Recorded on 2026-11-06 with Nextflow 26.04.6 and the real nf-schema 2.7.3, 2.8.0 and 3.0.0:
`NXF_OFFLINE=true nextflow run <script> -c <config> -ansi-log false` in a folder with these
inputs, a config that pins the version and includes `common.config` (or `summary.config`,
`summary2.config` for the `sum` and `sumt` outputs), and probe scripts that print each call's
result after `===== <name>`, or `!! <exception class>: <message>`. The probe folder is written
`<probe>`, the home folder `<masked>` or `<home>` and the user name `<user>`.
