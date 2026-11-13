"""Provides Snakemake file expansion, wildcard matching, flags, and named lists."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import collections
import collections.abc
import itertools
import json
import os
import re
import string
from pathlib import Path

from snakemake.exceptions import WildcardError, WorkflowError
from snakemake.iocontainers import (  # noqa: F401
    InputFiles,
    Log,
    Namedlist,
    OutputFiles,
    Params,
    ResourceList,
    Wildcards,
)

Resources = ResourceList

WILDCARD_REGEX = re.compile(
    r"""
    \{
        (?=(   # This lookahead assertion emulates an 'atomic group'
               # which is required for performance
            \s*(?P<name>[.\w]+)   # wildcard name
            (\s*,\s*
                (?P<constraint>                 # an optional constraint
                    ([^{}]+ | \{\d+(,\d+)?\})*  # allow curly braces to nest one level
                )                               # ...  as in '{w,a{3,5}}'
            )?\s*
        ))\1
    \}
    """,
    re.VERBOSE,
)


class AnnotatedString(str):
    """A text with flags, like the file of `temp("a.txt")`."""

    def __init__(self, value):
        self.flags = dict()


def flag(value, flag_type, flag_value=True):
    if isinstance(value, AnnotatedString):
        value.flags[flag_type] = flag_value
        return value
    if not isinstance(value, str) and isinstance(value, collections.abc.Iterable):
        return [flag(item, flag_type, flag_value=flag_value) for item in value]
    annotated = AnnotatedString(value)
    annotated.flags[flag_type] = flag_value
    return annotated


def is_flagged(value, flag_type):
    return isinstance(value, AnnotatedString) and bool(value.flags.get(flag_type))


def get_flag_value(value, flag_type):
    if isinstance(value, AnnotatedString):
        return value.flags.get(flag_type)
    return None


def ancient(value):
    return flag(value, "ancient")


def directory(value):
    return flag(value, "directory")


def temp(value, group_jobs=False):
    return flag(value, "temp")


temporary = temp


def protected(value):
    return flag(value, "protected")


def touch(value):
    return flag(value, "touch")


def pipe(value):
    return flag(value, "pipe")


def service(value):
    return flag(value, "service")


def local(value):
    return flag(value, "local")


def ensure(value, non_empty=False, sha256=None, md5=None, sha1=None):
    return flag(value, "ensure", {"non_empty": non_empty, "sha256": sha256})


def report(value, caption=None, category=None, subcategory=None, labels=None, patterns=None,
           htmlindex=None):
    return flag(value, "report", {"caption": caption, "category": category})


def unpack(value):
    return flag(value, "unpack")


def repeat(value, n_repeat):
    return flag(value, "repeat", n_repeat)


def multiext(prefix, *extensions, **named_extensions):
    if any("/" in ext or "\\" in ext for ext in itertools.chain(extensions, named_extensions.values())):
        raise WorkflowError(r"Extensions for multiext may not contain path delimiters (/,\).")
    if bool(extensions) == bool(named_extensions):
        raise WorkflowError(
            "multiext should be given with all named extensions or all not-named extensions, "
            "not a mix."
        )
    if extensions:
        return [flag(prefix + ext, "multiext", prefix) for ext in extensions]
    return [flag(prefix + ext, "multiext", prefix) for ext in named_extensions.values()]


def _wildcard_names(pattern):
    return re.findall(r"{([^}\.[!:]+)", pattern)


def expand(*args, **wildcard_values):
    """Fills the wildcards of file patterns with each combination of the given values."""
    filepatterns = args[0]
    combinator = args[1] if len(args) == 2 else itertools.product
    if isinstance(filepatterns, (str, Path)):
        filepatterns = [filepatterns]
    filepatterns = [str(pattern) if isinstance(pattern, Path) else pattern for pattern in filepatterns]
    format_dict = dict
    if wildcard_values.get("allow_missing") is True:

        class FormatDict(dict):
            def __missing__(self, key):
                return "{" + key + "}"

        format_dict = FormatDict
        if any("allow_missing" in _wildcard_names(pattern) for pattern in filepatterns):
            format_dict = dict
    values_of = {
        pattern: {
            key: value
            for key, value in wildcard_values.items()
            if key in _wildcard_names(pattern)
        }
        for pattern in filepatterns
    }

    def flatten(values):
        for wildcard, value in values.items():
            if isinstance(value, str) or not isinstance(value, collections.abc.Iterable) or (
                isinstance(value, tuple) and hasattr(value, "_fields")
            ):
                value = [value]
            yield [(wildcard, item) for item in value]

    formatter = string.Formatter()
    try:
        return [
            formatter.vformat(pattern, (), combination)
            for pattern in filepatterns
            for combination in map(format_dict, combinator(*flatten(values_of[pattern])))
        ]
    except KeyError as error:
        raise WildcardError("No values given for wildcard {}.".format(error))


collect = expand


def regex_from_filepattern(filepattern):
    parts = []
    last = 0
    wildcards = set()
    for match in WILDCARD_REGEX.finditer(filepattern):
        parts.append(re.escape(filepattern[last : match.start()]))
        wildcard = match.group("name")
        if wildcard in wildcards:
            if match.group("constraint"):
                raise ValueError(
                    "Constraint regex must be defined only in the first occurrence of the "
                    "wildcard in a string."
                )
            parts.append("(?P={})".format(wildcard))
        else:
            wildcards.add(wildcard)
            parts.append(
                "(?P<{}>{})".format(wildcard, match.group("constraint") or ".+")
            )
        last = match.end()
    parts.append(re.escape(filepattern[last:]))
    parts.append("$")
    return "".join(parts)


def glob_wildcards(pattern, files=None, followlinks=False):
    """The values of the wildcards of a pattern in the files that match it."""
    pattern = os.path.normpath(pattern)
    first_wildcard = re.search("{[^{]", pattern)
    dirname = (
        os.path.dirname(pattern[: first_wildcard.start()])
        if first_wildcard
        else os.path.dirname(pattern)
    )
    if not dirname:
        dirname = "."
    found = [match.group("name") for match in WILDCARD_REGEX.finditer(pattern)]
    names = sorted(set(found), key=found.index)
    wildcards = collections.namedtuple("Wildcards", names)(*[list() for _ in names])
    regex = re.compile(regex_from_filepattern(pattern))
    if files is None:
        files = (
            os.path.normpath(os.path.join(dirpath, name))
            for dirpath, dirnames, filenames in os.walk(dirname, followlinks=followlinks)
            for name in itertools.chain(filenames, dirnames)
        )
    for name in files:
        match = re.match(regex, name)
        if match:
            for key, value in match.groupdict().items():
                getattr(wildcards, key).append(value)
    return wildcards


def strip_wildcard_constraints(pattern):
    return WILDCARD_REGEX.sub(lambda match: "{{{}}}".format(match.group("name")), pattern)


def limit(pattern, **wildcards):
    return pattern.format(
        **{
            wildcard: "{{{},{}}}".format(wildcard, "|".join(values))
            for wildcard, values in wildcards.items()
        }
    )


def load_configfile(configpath):
    with open(configpath) as handle:
        if str(configpath).endswith(".json"):
            return json.load(handle, object_pairs_hook=collections.OrderedDict)
        import yaml

        return yaml.safe_load(handle)
