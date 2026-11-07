# A one-node SLURM cluster for tests: slurmctld, slurmd and munge in one container, with the
# Docker client for tasks that run on the Docker engine of the host.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends slurmctld slurmd slurm-client munge \
        docker-cli ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && { [ -s /etc/munge/munge.key ] || mungekey --create; } \
    && chown munge:munge /etc/munge/munge.key && chmod 600 /etc/munge/munge.key \
    && mkdir -p /var/lib/slurm/slurmctld /var/lib/slurm/slurmd /var/log/slurm \
    && chown -R slurm:slurm /var/lib/slurm /var/log/slurm
# The node takes its CPUs and memory from this file, not from the machine.
RUN printf '%s\n' \
    ClusterName=uwe SlurmctldHost=uwe-qualify-slurm SlurmUser=slurm AuthType=auth/munge \
    ProctrackType=proctrack/linuxproc TaskPlugin=task/none JobAcctGatherType=jobacct_gather/none \
    AccountingStorageType=accounting_storage/none SelectType=select/cons_tres \
    SelectTypeParameters=CR_Core SlurmdParameters=config_overrides ReturnToService=2 \
    StateSaveLocation=/var/lib/slurm/slurmctld SlurmdSpoolDir=/var/lib/slurm/slurmd \
    SlurmctldLogFile=/var/log/slurm/slurmctld.log SlurmdLogFile=/var/log/slurm/slurmd.log \
    'NodeName=uwe-qualify-slurm CPUs=8 RealMemory=16000 State=UNKNOWN' \
    'PartitionName=debug Nodes=ALL Default=YES MaxTime=INFINITE State=UP' \
    > /etc/slurm/slurm.conf \
    && echo CgroupPlugin=disabled > /etc/slurm/cgroup.conf
CMD ["bash", "-c", "install -d -o munge -g munge /run/munge && runuser -u munge -- munged && slurmctld && slurmd && exec sleep infinity"]
