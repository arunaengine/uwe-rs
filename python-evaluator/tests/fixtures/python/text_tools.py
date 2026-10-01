"""Exercises string formatting, CSV parsing, path methods, errors, and threads."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import csv
import errno
import io
import os
import queue
import string
import threading
from pathlib import Path

template = string.Template("$sample/${unit}_$$x")
print(template.substitute(sample="A", unit=1), template.safe_substitute({"sample": "B"}), template.template)
try:
    template.substitute(sample="A")
except KeyError as error:
    print("KeyError", error)
print(list(string.Formatter().parse("{a}/{b.c}-x{d[0]:>3}!")))
print(string.Formatter().format("{0}-{name}", 1, name="n"))

sample = "id;value\na;1\nb;2\n"
dialect = csv.Sniffer().sniff(sample)
print(dialect.delimiter, csv.Sniffer().has_header(sample))
print([row for row in csv.reader(io.StringIO(sample), dialect)])
reader = csv.DictReader(io.StringIO("ID ;Value\nx;1\n"), delimiter=";")
reader.fieldnames = [name.strip().lower() for name in reader.fieldnames]
print(reader.fieldnames, [dict(row) for row in reader])

print(os.strerror(errno.ENOENT), os.strerror(errno.EACCES))
print(Path.joinpath(Path("a"), "b.txt"), str(Path.absolute(Path("/x/y"))), Path.with_suffix(Path("r.txt"), ".csv"))

items = queue.Queue()
def fill():
    for index in range(3):
        items.put(f"f{index}.txt")
worker = threading.Thread(target=fill)
worker.start()
worker.join()
print(items.qsize(), items.get(), worker.is_alive())
