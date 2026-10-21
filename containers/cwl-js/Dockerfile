# Packages the trusted native CWL JavaScript worker and its runtime libraries.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
FROM scratch
COPY rootfs/ /
ENV UWE_CWL_WORKER=1
USER 65534:65534
WORKDIR /tmp
ENTRYPOINT ["/usr/local/bin/iunctus"]
CMD ["__cwl-js-worker"]
