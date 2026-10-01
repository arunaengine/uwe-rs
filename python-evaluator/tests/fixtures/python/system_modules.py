"""Exercises process, file, user, queue, and threading stand-ins."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import filecmp
import fileinput
import pwd
import queue
import threading
import urllib.error
from urllib.parse import quote, quote_plus, unquote, urlencode, urljoin, urlparse, urlsplit

parts = urlparse("https://User@Example.org:8443/data/file.txt;v=1?x=1&y=2#top")
print(parts.scheme, parts.netloc, parts.path, parts.params, parts.query, parts.fragment)
print(parts.hostname, parts.port, parts.username, parts.geturl())
print(tuple(urlsplit("s3://bucket/key/part")), urlparse("data/local.txt").path, urlparse("file:///tmp/a").netloc == "")
print(urljoin("https://a.org/x/y.html", "z.html"), urljoin("https://a.org/x/", "../w"), urljoin("https://a.org/x/", "/abs"))
print(quote("a b/c&d"), quote_plus("a b/c"), unquote("a%20b%2Fc"), urlencode({"q": "a b", "n": 1}))
print(issubclass(urllib.error.HTTPError, urllib.error.URLError))

jobs = queue.Queue()
jobs.put(1)
jobs.put(2)
print(jobs.qsize(), jobs.get(), jobs.empty(), jobs.get(), jobs.empty())
try:
    jobs.get_nowait()
except queue.Empty:
    print("empty")

lock = threading.Lock()
with lock:
    print("locked", lock.locked())
print("free", lock.locked())

lines = [line.strip() for line in fileinput.input(["containers.out", "numbers.out"])]
fileinput.close()
print(len(lines) > 10, lines[0] == open("containers.out").readline().strip())
print(filecmp.cmp("containers.out", "containers.out"), filecmp.cmp("containers.out", "numbers.out"))
print(type(pwd.getpwuid(0)[0]).__name__)
import tarfile
print(tarfile.is_tarfile("archive.tar.gz"), tarfile.is_tarfile("containers.out"))
