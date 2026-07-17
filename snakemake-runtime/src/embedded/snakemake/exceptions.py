# The exceptions of snakemake that the code of jobs raises or catches.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


def _format_arguments(args):
    parts = []
    for arg in args:
        if isinstance(arg, BaseException):
            parts.append("{}: {}".format(type(arg).__name__, arg))
        else:
            parts.append(str(arg))
    return "\n".join(parts)


class WorkflowError(Exception):
    def __init__(self, *args, lineno=None, snakefile=None, rule=None):
        super().__init__(_format_arguments(args))
        self.lineno = lineno
        self.snakefile = snakefile
        self.rule = rule


class WildcardError(Exception):
    pass


class IncompleteCheckpointException(Exception):
    def __init__(self, rule=None, targetfile=None):
        super().__init__("Checkpoint {} is incomplete".format(rule))
        self.rule = rule
        self.targetfile = targetfile
