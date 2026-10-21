"""Provides Snakemake formatters and file and configuration helpers for job code."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import collections.abc
import inspect
import multiprocessing
import os
import re
import shlex
import string

from snakemake.exceptions import WorkflowError
from snakemake.iocontainers import Namedlist, Wildcards


class SequenceFormatter(string.Formatter):
    """Formats each item of a list, tuple or set and joins them with a separator."""

    def __init__(self, separator=" ", element_formatter=string.Formatter(), *args, **kwargs):
        self.separator = separator
        self.element_formatter = element_formatter

    def format_element(self, elem, format_spec):
        return self.element_formatter.format_field(elem, format_spec)

    def format_field(self, value, format_spec):
        if isinstance(value, Wildcards):
            return ",".join(
                "{}={}".format(name, item)
                for name, item in sorted(value.items(), key=lambda item: item[0])
            )
        if isinstance(value, (list, tuple, set, frozenset)):
            return self.separator.join(self.format_element(v, format_spec) for v in value)
        return self.format_element(value, format_spec)


class QuotedFormatter(string.Formatter):
    """Quotes a field for the shell when its format ends with `q`."""

    def __init__(self, quote_func=None, *args, **kwargs):
        if quote_func is None:
            quote_func = shlex.quote
        self.quote_func = quote_func
        super().__init__(*args, **kwargs)

    def format_field(self, value, format_spec):
        if format_spec.endswith("u"):
            format_spec = format_spec[:-1]
            do_quote = False
        else:
            do_quote = format_spec.endswith("q")
            if do_quote:
                format_spec = format_spec[:-1]
        formatted = super().format_field(value, format_spec)
        if do_quote and formatted != "":
            formatted = self.quote_func(formatted)
        return formatted


class AlwaysQuotedFormatter(QuotedFormatter):
    """Quotes every field unless its format ends with `u`."""

    def format_field(self, value, format_spec):
        if not format_spec.endswith("q") and not format_spec.endswith("u"):
            format_spec += "q"
        return super().format_field(value, format_spec)


def format(_pattern, *args, stepout=1, _quote_all=False, quote_func=None, **kwargs):
    """Formats a pattern with the variables of the caller `stepout` frames up, as Snakemake
    formats shell commands."""
    frame = inspect.currentframe().f_back
    while stepout > 1:
        if not frame.f_back:
            break
        frame = frame.f_back
        stepout -= 1
    variables = dict(frame.f_globals)
    variables.update(frame.f_locals)
    variables.update(kwargs)
    formatter = SequenceFormatter(separator=" ")
    if _quote_all:
        formatter.element_formatter = AlwaysQuotedFormatter(quote_func)
    else:
        formatter.element_formatter = QuotedFormatter(quote_func)
    try:
        return formatter.format(_pattern, *args, **variables)
    except KeyError as error:
        name = str(error).strip("'")
        if "wildcards" in variables and name in variables["wildcards"].keys():
            raise NameError(
                "The name '{0}' is unknown in this context. "
                "Did you mean 'wildcards.{0}'?".format(name)
            )
        raise NameError(
            "The name {} is unknown in this context. Please make sure that you defined that "
            "variable. Also note that braces not used for variable access have to be escaped "
            "by repeating them, i.e. {{{{print $1}}}}".format(str(error))
        )


def makedirs(dirnames):
    if isinstance(dirnames, str):
        dirnames = [dirnames]
    for dirname in dirnames:
        os.makedirs(dirname, exist_ok=True)


def linecount(filename):
    with open(filename) as handle:
        return sum(1 for _ in handle)


def simplify_path(path):
    relpath = os.path.relpath(path)
    return path if relpath.startswith("../../") else relpath


def update_config(config, overwrite_config):
    def update(config, overwrite_config):
        for key, value in overwrite_config.items():
            if not isinstance(config, collections.abc.Mapping):
                config = {}
            if isinstance(value, collections.abc.Mapping):
                config[key] = update(config.get(key, {}), value)
            else:
                config[key] = value
        return config

    update(config, overwrite_config)


def available_cpu_count():
    try:
        return len(os.sched_getaffinity(0))
    except AttributeError:
        return multiprocessing.cpu_count()


def min_version(version):
    """Snakemake 9.27 is what uwe follows; a later minimum is an error."""
    from snakemake import __version__

    def parts(text):
        return [int(part) if part.isdigit() else 0 for part in re.split(r"[.+-]", text)[:3]]

    if parts(__version__) < parts(version):
        raise WorkflowError(
            "Expecting Snakemake version {} or higher (you are currently using {}).".format(
                version, __version__
            )
        )


def listfiles(pattern, restriction=None, omit_value=None):
    from snakemake.io import regex_from_filepattern

    pattern = os.path.normpath(pattern)
    first_wildcard = re.search("{[^{]", pattern)
    if first_wildcard:
        dirname = os.path.dirname(pattern[: first_wildcard.start()]) or "."
    else:
        dirname = os.path.dirname(pattern)
    regex = re.compile(regex_from_filepattern(pattern))
    for dirpath, dirnames, filenames in os.walk(dirname):
        for name in filenames + dirnames:
            if dirpath != ".":
                name = os.path.normpath(os.path.join(dirpath, name))
            match = re.match(regex, name)
            if match:
                wildcards = Namedlist(fromdict=match.groupdict())
                if restriction is not None:
                    invalid = any(
                        omit_value not in value and value != wildcards[key]
                        for key, value in restriction.items()
                    )
                    if not invalid:
                        yield name, wildcards
                else:
                    yield name, wildcards


def read_job_properties(jobscript, prefix="# properties", pattern=re.compile("# properties = (.*)")):
    import json

    with open(jobscript) as handle:
        for line in handle:
            match = pattern.match(line)
            if match:
                return json.loads(match.group(1))


def argvquote(arg, force=True):
    if not force and len(arg) != 0 and not any(c in arg for c in ' \t\n\v"'):
        return arg
    backslashes = 0
    quoted = '"'
    for char in arg:
        if char == "\\":
            backslashes += 1
            continue
        if char == '"':
            quoted += (backslashes * 2 + 1) * "\\"
        else:
            quoted += backslashes * "\\"
        backslashes = 0
        quoted += char
    quoted += backslashes * 2 * "\\" + '"'
    return quoted


def validate(data, schema, set_default=True):
    """Checks a dict, or each row of a pandas table, against a JSON schema file, with the
    jsonschema package of the image, and fills in defaults as Snakemake does."""
    import json

    try:
        import jsonschema
    except ImportError:
        raise WorkflowError(
            "The Python package jsonschema must be installed to use the validate function"
        )
    with open(schema) as handle:
        if schema.endswith((".yaml", ".yml")):
            import yaml

            schema = yaml.safe_load(handle)
        else:
            schema = json.load(handle)
    validator_class = jsonschema.validators.validator_for(schema)
    validator = validator_class(schema)

    def fill(record):
        if set_default:
            for name, spec in schema.get("properties", {}).items():
                if "default" in spec and name not in record:
                    record[name] = spec["default"]

    try:
        import pandas as pd
    except ImportError:
        pd = None
    if pd is not None and isinstance(data, pd.DataFrame):
        for record in data.to_dict("records"):
            validator.validate({key: value for key, value in record.items() if not pd.isnull(value)})
        if set_default:
            for name, spec in schema.get("properties", {}).items():
                if "default" in spec:
                    if name not in data:
                        data[name] = spec["default"]
                    else:
                        data[name] = data[name].fillna(spec["default"])
        return
    try:
        validator.validate(data)
    except jsonschema.ValidationError as error:
        raise WorkflowError("Error validating config file.", error)
    fill(data)
