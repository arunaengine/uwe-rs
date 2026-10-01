"""Provides Snakemake 9.27 named lists, job context, shell log formatting, and plain params."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib
import os
import urllib.parse

FILE_HASH_PREFIX_LEN = 16


class AttributeGuard:
    """Stands for `index()` and `sort()`, which may also be names of items."""

    def __init__(self, name):
        self.name = name

    def __call__(self, *args, **kwargs):
        raise AttributeError(
            "{}() cannot be used on snakemake input, output, resources etc.; instead it is "
            "a valid name for items on those objects. If you want e.g. to sort, convert to "
            "a plain list before or directly use sorted() on the object.".format(self.name)
        )


class Namedlist(list):
    """A list whose items or ranges of items also have names, as attributes and keys."""

    def __init__(
        self,
        toclone=None,
        fromdict=None,
        plainstr=False,
        strip_constraints=False,
        custom_map=None,
    ):
        list.__init__(self)
        self._names = dict()
        self._allowed_overrides = ["index", "sort"]
        for name in self._allowed_overrides:
            setattr(self, name, AttributeGuard(name))
        if toclone is not None:
            if custom_map is not None:
                self.extend(map(custom_map, toclone))
            elif plainstr:
                self.extend(str(item) for item in toclone)
            else:
                self.extend(toclone)
            if isinstance(toclone, Namedlist):
                self._take_names(toclone._get_names())
        if fromdict is not None:
            for key, item in fromdict.items():
                self.append(item)
                self._add_name(key)

    def _add_name(self, name):
        self._set_name(name, len(self) - 1)

    def _set_name(self, name, index, end=None):
        if name not in self._allowed_overrides and hasattr(self.__class__, name):
            raise AttributeError(
                "invalid name for input, output, wildcard, params or log: {name} is "
                "reserved for internal use".format(name=name)
            )
        self._names[name] = (index, end)
        if end is None:
            setattr(self, name, self[index])
        else:
            setattr(self, name, Namedlist(toclone=self[index:end]))

    def update(self, items):
        for key, value in items.items():
            if key in self._names:
                raise ValueError("Key {} already exists in Namedlist".format(key))
            self.append(value)
            self._add_name(key)

    def _get_names(self):
        for name, index in self._names.items():
            yield name, index

    def _take_names(self, names):
        for name, (start, end) in names:
            self._set_name(name, start, end=end)

    def items(self):
        for name in self._names:
            yield name, getattr(self, name)

    def _allitems(self):
        following = 0
        ordered = sorted(
            self._names.items(),
            key=lambda item: (
                item[1][0],
                item[1][0] + 1 if item[1][1] is None else item[1][1],
            ),
        )
        for name, (start, end) in ordered:
            if end is None:
                end = start + 1
            if start > following:
                for item in self[following:start]:
                    yield None, item
            yield name, getattr(self, name)
            following = end
        for item in self[following:]:
            yield None, item

    def keys(self):
        return self._names.keys()

    def _plainstrings(self):
        return self.__class__(toclone=self, plainstr=True)

    def _clone(self):
        return self.__class__(toclone=self)

    def get(self, key, default_value=None):
        value = self.__dict__.get(key, default_value)
        if isinstance(value, AttributeGuard):
            return default_value
        return value

    def __getitem__(self, key):
        if isinstance(key, str):
            return getattr(self, key)
        return super().__getitem__(key)

    def __hash__(self):
        return hash(tuple(self))

    def __str__(self):
        return " ".join(map(str, self))


def _file_sizes(files):
    return [os.path.getsize(path) for path in files if os.path.exists(path)]


class InputFiles(Namedlist):
    @property
    def size_files(self):
        return _file_sizes(self)

    @property
    def size_files_kb(self):
        return [size / 1024 for size in self.size_files]

    @property
    def size_files_mb(self):
        return [size / 1024 for size in self.size_files_kb]

    @property
    def size_files_gb(self):
        return [size / 1024 for size in self.size_files_mb]

    @property
    def size(self):
        return sum(self.size_files)

    @property
    def size_kb(self):
        return sum(self.size_files_kb)

    @property
    def size_mb(self):
        return sum(self.size_files_mb)

    @property
    def size_gb(self):
        return sum(self.size_files_gb)


class OutputFiles(Namedlist):
    pass


class Wildcards(Namedlist):
    pass


class Params(Namedlist):
    pass


class ResourceList(Namedlist):
    pass


class Log(Namedlist):
    pass


def named(kind, values, names):
    """A Namedlist of a kind with its values and its names, each a name, the position of its
    item and, for a name of several items, the end of their range."""
    result = kind(toclone=values)
    for name, start, end in names:
        result._set_name(name, start, end)
    return result


class Unavailable:
    """A value that iunctus could not hand to the code of a job, like a function in the params
    of a rule. Using it raises the error that says why."""

    def __init__(self, message):
        self._message = message

    def _fail(self, *args, **kwargs):
        raise TypeError(self._message)

    __call__ = __iter__ = __len__ = __getitem__ = __str__ = __bool__ = _fail

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)
        self._fail()

    def __repr__(self):
        return "<unavailable: {}>".format(self._message)


class ReportHref:
    def __init__(self, path, parent=None, url_args=None, anchor=None):
        self._parent = parent
        if parent is None:
            self._id = hashlib.sha256(str(path).encode()).hexdigest()
        else:
            self._id = parent._id
        self._path = path if isinstance(path, str) else str(path.as_posix())
        self._url_args = dict(url_args) if url_args else {}
        self._anchor = anchor

    def child_path(self, path):
        return self.__class__(path, parent=self)

    def url_args(self, **args):
        return self.__class__(path=self._path, parent=self._parent, url_args=args)

    def anchor(self, anchor):
        return self.__class__(
            path=self._path, parent=self._parent, url_args=self._url_args, anchor=anchor
        )

    def __str__(self):
        path = os.path.basename(self._path) if self._parent is None else self._path
        args = ""
        if self._url_args:
            args = "?" + "&".join(
                "{}={}".format(key, urllib.parse.quote(str(value)))
                for key, value in self._url_args.items()
            )
        anchor = "#" + urllib.parse.quote(self._anchor) if self._anchor else ""
        return "../{}/{}{}{}".format(self._id[:FILE_HASH_PREFIX_LEN], path, args, anchor)


class Snakemake:
    """The `snakemake` object of scripts, wrappers and notebooks."""

    def __init__(
        self,
        input_,
        output,
        params,
        wildcards,
        threads,
        resources,
        log,
        config,
        rulename,
        bench_iteration,
        scriptdir=None,
    ):
        self.input = input_._plainstrings()
        self.output = output._plainstrings()
        self._params_store = Params(toclone=params)
        self._params_types = dict()
        self.wildcards = wildcards
        self.threads = threads
        self.resources = resources
        self.log = log._plainstrings()
        self.config = config
        self.rule = rulename
        self.bench_iteration = bench_iteration
        self.scriptdir = scriptdir

    def report_href(self, path):
        return ReportHref(path)

    def _log_shell_redirect(self, log, stdout=True, stderr=True, append=False):
        if not log:
            return ""
        lookup = {
            (True, True, True): " >> {0} 2>&1",
            (True, False, True): " >> {0}",
            (False, True, True): " 2>> {0}",
            (True, True, False): " > {0} 2>&1",
            (True, False, False): " > {0}",
            (False, True, False): " 2> {0}",
        }
        return lookup[(stdout, stderr, append)].format(str(log))

    def log_fmt_shell(self, stdout=True, stderr=True, append=False):
        return self._log_shell_redirect(str(self.log), stdout, stderr, append)

    @property
    def params(self):
        params = Params(toclone=list(self._params_store))
        try:
            for index, value in enumerate(params):
                kind = self._params_types.get(index)
                if kind is None:
                    continue
                if kind.startswith("pd."):
                    import pandas as pd

                    if kind == "pd.DataFrame":
                        params[index] = pd.DataFrame.from_dict(value)
                    elif kind == "pd.Series":
                        params[index] = pd.Series(value)
                elif kind == "np.ndarray":
                    import numpy as np

                    params[index] = np.array(value)
                elif kind.startswith("pl."):
                    import polars as pl

                    if kind == "pl.LazyFrame":
                        params[index] = pl.from_dict(value).lazy()
                    elif kind == "pl.DataFrame":
                        params[index] = pl.from_dict(value)
                    elif kind == "pl.Series":
                        params[index] = pl.Series(**value)
        except ImportError as error:
            raise ImportError(
                "Failed to import required module for loading rule params. Make sure that "
                "the respective package (numpy, pandas, polars) is available in the software "
                "environment in which the script/wrapper/notebook is executed: {}".format(error)
            )
        params._take_names(self._params_store._get_names())
        return params
