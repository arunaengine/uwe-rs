"""The nf_python module that uwe puts beside each nf-python call: arguments in, results out."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import datetime
import decimal
import json
import math
import os
import pathlib
import sys

__all__ = ["MemoryUnit", "VersionNumber", "nf"]


class MemoryUnit:
    """A size in bytes, as Nextflow's memory unit."""

    def __init__(self, size):
        self.bytes = int(size)

    @property
    def kilo(self):
        return self.bytes // 1024

    @property
    def mega(self):
        return self.bytes // 1024**2

    @property
    def giga(self):
        return self.bytes // 1024**3

    def __repr__(self):
        return f"MemoryUnit({self.bytes})"


class VersionNumber:
    """A version as Nextflow's version number: major, minor and patch."""

    def __init__(self, major, minor, patch):
        self.major = major
        self.minor = minor
        self.patch = patch

    def __repr__(self):
        return f"VersionNumber({self.major}, {self.minor}, {self.patch})"


def unpack(pair):
    """The Python value of a typed pair of the input file."""
    if not isinstance(pair, list) or len(pair) != 2:
        raise ValueError(f"Expected a list of size 2 for unpacking, got: {pair}")
    tag, value = pair
    if tag == "Null":
        return None
    if tag in ("List", "Bag"):
        return [unpack(item) for item in value]
    if tag == "Set":
        return {unpack(item) for item in value}
    if tag == "Map":
        return {unpack(key): unpack(item) for key, item in value}
    if tag == "String":
        return str(value)
    if tag == "Integer":
        return int(value)
    if tag == "Decimal":
        return decimal.Decimal(str(value))
    if tag == "Float":
        return float(value)
    if tag == "Boolean":
        return bool(value)
    if tag == "Path":
        return pathlib.Path(value)
    if tag == "Duration":
        return datetime.timedelta(milliseconds=value)
    if tag == "MemoryUnit":
        return MemoryUnit(value)
    if tag == "VersionNumber":
        return VersionNumber(*value)
    raise ValueError(f"Unknown type from Nextflow: {tag}")


def pack(value):
    """The typed pair of a Python value; booleans are checked before integers."""
    if value is None:
        return ["Null", None]
    if isinstance(value, bool):
        return ["Boolean", value]
    if isinstance(value, int):
        return ["Integer", value]
    if isinstance(value, float):
        if math.isnan(value):
            return ["Float", "nan"]
        if math.isinf(value):
            return ["Float", "inf" if value > 0 else "-inf"]
        return ["Float", value]
    if isinstance(value, decimal.Decimal):
        return ["Decimal", str(value)]
    if isinstance(value, str):
        return ["String", value]
    if isinstance(value, pathlib.PurePath):
        return ["Path", str(value)]
    if isinstance(value, datetime.timedelta):
        return ["Duration", value // datetime.timedelta(milliseconds=1)]
    if isinstance(value, MemoryUnit):
        return ["MemoryUnit", value.bytes]
    if isinstance(value, VersionNumber):
        return ["VersionNumber", [value.major, value.minor, value.patch]]
    if isinstance(value, (list, tuple)):
        return ["List", [pack(item) for item in value]]
    if isinstance(value, (set, frozenset)):
        return ["Set", [pack(item) for item in value]]
    if isinstance(value, dict):
        return ["Map", [[pack(key), pack(item)] for key, item in value.items()]]
    raise TypeError(f"Cannot serialize object of type {type(value)}")


class Nextflow:
    """The arguments of the call, and the output that goes back to the workflow."""

    def __init__(self):
        infile = os.environ.get("NEXTFLOW_INFILE")
        outfile = os.environ.get("NEXTFLOW_OUTFILE")
        if not infile or not outfile:
            raise RuntimeError("NEXTFLOW_INFILE and NEXTFLOW_OUTFILE env vars must be set.")
        self.written = False
        self.outfile = outfile
        try:
            with open(infile, encoding="utf-8") as handle:
                self.args = unpack(json.load(handle))
        except Exception as error:
            print(f"[nextflow.py] Failed to load input arguments: {error}", file=sys.stderr)
            self.args = (None, None)

    def output(self, *values, **named):
        """Writes the values, or the named values as a map, replacing an earlier output."""
        if values and named:
            raise ValueError("Cant pass both unnamed outputs and named outputs!")
        if not values and not named:
            return
        packed = pack(named) if named else pack(list(values))
        with open(self.outfile, "w", encoding="utf-8") as handle:
            json.dump(packed, handle, separators=(",", ":"))
        self.written = True

    def __del__(self):
        if not getattr(self, "written", True):
            raise RuntimeWarning("Output not written before script exit.")


nf = Nextflow()
