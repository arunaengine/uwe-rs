# shell() of Snakemake 9.27 for the code of jobs: it formats the command with the variables
# of its caller, runs it with bash (`set -euo pipefail; ` in front) in the job's container,
# and sets the thread variables of numerical libraries to the job's threads.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import inspect
import os
import shutil
import subprocess as sp
import sys

from snakemake.exceptions import WorkflowError
from snakemake.utils import format

# The marker that the function of a `run:` block holds among its locals.
RULEFUNC_CONTEXT_MARKER = "__is_snakemake_rule_func"

THREAD_VARIABLES = (
    "OMP_NUM_THREADS",
    "GOTO_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "MKL_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
)


class shell:
    _process_args = {}
    _process_prefix = None
    _process_suffix = ""

    @classmethod
    def get_executable(cls):
        return cls._process_args.get("executable", None)

    @classmethod
    def check_output(cls, cmd, **kwargs):
        return sp.check_output(cmd, shell=True, executable=cls.get_executable(), **kwargs)

    @classmethod
    def executable(cls, cmd):
        if cmd is not None:
            cmd = str(cmd)
        if cmd and not os.path.isabs(cmd):
            found = shutil.which(cmd)
            if not found:
                raise WorkflowError(
                    "Cannot set default shell {} because it is not available in your "
                    "PATH.".format(cmd)
                )
            cmd = found
        cls._process_args["executable"] = cmd

    @classmethod
    def _executable_name(cls, executable=None):
        executable = executable or cls.get_executable()
        return os.path.split(executable)[-1].lower() if executable else None

    @classmethod
    def _get_process_prefix(cls, executable=None):
        if cls._executable_name(executable) == "bash" and cls._process_prefix is None:
            return "set -euo pipefail; "
        return cls._process_prefix or ""

    @classmethod
    def prefix(cls, prefix):
        cls._process_prefix = format(prefix, stepout=2)

    @classmethod
    def suffix(cls, suffix):
        cls._process_suffix = format(suffix, stepout=2)

    def __new__(cls, cmd, *args, iterable=False, read=False, bench_record=None, **kwargs):
        if "stepout" in kwargs:
            raise KeyError("Argument stepout is not allowed in shell command.")
        cmd = format(cmd, *args, stepout=2, **kwargs)
        caller = inspect.currentframe().f_back.f_locals
        if caller.get(RULEFUNC_CONTEXT_MARKER):
            context = dict(caller)
        elif "snakemake" in caller and hasattr(caller["snakemake"], "threads"):
            context = {"threads": caller["snakemake"].threads}
        else:
            context = dict()
        context.update(kwargs)
        resources = context.get("resources", {}) or {}
        executable = resources.get("shell_exec") if hasattr(resources, "get") else None
        if executable is None:
            executable = cls.get_executable()
        cmd = " ".join((cls._get_process_prefix(executable), cmd, cls._process_suffix)).strip()
        environment = dict(os.environ)
        threads = str(context.get("threads", 1))
        for name in THREAD_VARIABLES:
            environment[name] = threads
        tmpdir = resources.get("tmpdir") if hasattr(resources, "get") else None
        if tmpdir:
            for name in ("TMPDIR", "TMP", "TEMPDIR", "TEMP"):
                environment[name] = tmpdir
        extra = kwargs.get("additional_envvars")
        if extra is not None:
            if not isinstance(extra, dict) or not all(isinstance(v, str) for v in extra.values()):
                raise WorkflowError(
                    "Given environment variables for shell command have to be a dict of "
                    "strings, but the following was provided instead:\n{}".format(extra)
                )
            environment.update(extra)
        stdout = sp.PIPE if iterable or read else None
        process = sp.Popen(
            cmd,
            bufsize=-1,
            shell=True,
            stdout=stdout,
            universal_newlines=iterable or read or None,
            close_fds=True,
            executable=executable,
            env=environment,
        )
        if iterable:
            return cls._iterate(process, cmd)
        result = process.stdout.read() if read else None
        code = process.wait()
        if code:
            raise sp.CalledProcessError(code, cmd)
        return result

    @staticmethod
    def _iterate(process, cmd):
        for line in process.stdout:
            yield line[:-1]
        code = process.wait()
        if code:
            raise sp.CalledProcessError(code, cmd)


# bash is the shell, as on every POSIX system Snakemake runs on; sh where there is no bash.
if shutil.which("bash"):
    shell.executable("bash")
elif shutil.which("sh"):
    print("Cannot set bash as default shell because it is not available in your PATH. "
          "Falling back to sh.", file=sys.stderr)
    shell.executable("sh")
