# Library modules.
import os
import re
import json
import itertools
import functools
import collections
import math
from os.path import join, basename, dirname, splitext
from pathlib import Path

print(join("a", "b", "c.txt"), join("/x", "/abs"), basename("/a/b.txt"), dirname("/a/b.txt"), splitext("a/b.tar.gz"))
print(os.path.normpath("a//b/../c/./d"), os.path.split("a/b/"), os.path.splitext(".bashrc"), os.path.isabs("/x"))
print(os.path.relpath("/a/b/c", "/a"), os.path.commonprefix(["abc", "abd"]), os.sep)
p = Path("data") / "sample.fastq.gz"
print(p, p.name, p.stem, p.suffix, p.suffixes, p.parent, str(p.with_suffix(".bam")), p.parts, Path("a/b/").name)
m = re.match(r"(?P<sample>\w+)_(?P<unit>\d+)\.fq", "S1_2.fq")
print(m.group("sample"), m.group(2), m.groups(), m.groupdict(), m.span(), bool(re.search("x", "abc")))
print(re.sub(r"(\d+)", r"<\1>", "a1b22"), re.findall(r"\d", "a1b2"), re.split(r"[,;]", "a,b;c"), re.fullmatch("a+", "aaa") is not None)
print(re.sub("a", lambda m: m.group(0).upper(), "banana"), re.escape("a.b"), re.compile(r"x{2}").search("axxb").group())
print(re.findall(r"(\w)(\d)", "a1 b2"), re.match("^{sample}$", "{sample}") is not None, re.subn("o", "0", "foo"))
data = json.loads('{"b": [1, 2.5, true, null], "a": "text"}')
print(data, json.dumps(data), json.dumps(data, indent=2, sort_keys=True))
print(list(itertools.product([1, 2], "ab")), list(itertools.chain([1], [2, 3])), list(itertools.combinations("abc", 2)))
print([(k, list(g)) for k, g in itertools.groupby("aabbbc")], list(itertools.islice(itertools.count(5), 3)))
print(list(itertools.permutations([1, 2, 3], 2))[:3], list(itertools.zip_longest([1, 2], [3], fillvalue=0)))
print(functools.reduce(lambda a, b: a * b, [1, 2, 3, 4]), functools.partial(int, base=2)("101"))
dd = collections.defaultdict(list)
dd["x"].append(1)
dd["x"].append(2)
c = collections.Counter("abracadabra")
print(dict(dd), c.most_common(2), c["z"], collections.OrderedDict([("a", 1)]))
Point = collections.namedtuple("Point", ["x", "y"])
pt = Point(1, y=2)
print(pt, pt.x, pt[1], pt._asdict(), tuple(pt), pt == (1, 2))
print(math.ceil(2.1), math.floor(-2.1), math.sqrt(16), math.log(8, 2), math.isnan(float("nan")), math.inf > 1e308, math.pi)
print(math.log10(1000), math.prod([2, 3]), math.gcd(12, 18), math.isclose(0.1 + 0.2, 0.3))
print(sorted(set(os.path.basename(f) for f in ["x/a.txt", "y/a.txt", "b.txt"])))
import platform
try:
    platform.linux_distribution()
except AttributeError as error:
    print("AttributeError:", error)
print(platform.mac_ver(), platform.win32_ver(), platform.python_implementation())
import configparser, email, imaplib, smtplib
from email.message import EmailMessage
from collections import UserDict
from tkinter import E
print(EmailMessage.__name__, configparser.ConfigParser.__name__, UserDict({"a": 1})["a"], E)
import csv, io
print(csv.DictReader(io.StringIO("")).fieldnames, csv.DictReader(io.StringIO("a,b\n1,2\n")).fieldnames)
with open("library.py") as source:
    print(next(source).rstrip(), next(source, "end") != "end")
import resource
print(resource.RLIMIT_NOFILE, resource.getrlimit(resource.RLIMIT_NOFILE)[0] > 0, resource.setrlimit(resource.RLIMIT_NOFILE, resource.getrlimit(resource.RLIMIT_NOFILE)))
import contextlib, logging


@contextlib.contextmanager
def opened(name) -> "logging.Logger":
    yield name


print(callable(opened), logging.Logger.__name__)
from git import Repo
from git.exc import InvalidGitRepositoryError
import git
print(issubclass(InvalidGitRepositoryError, Exception), git.exc.InvalidGitRepositoryError.__name__)
