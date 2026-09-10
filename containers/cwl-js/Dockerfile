# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
FROM scratch
COPY rootfs/ /
ENV IUNCTUS_CWL_WORKER=1
USER 65534:65534
WORKDIR /tmp
ENTRYPOINT ["/usr/local/bin/iunctus"]
CMD ["__cwl-js-worker"]
