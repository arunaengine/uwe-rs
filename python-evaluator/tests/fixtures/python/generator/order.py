"""Checks that the parts before a yield are evaluated before the generator suspends."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


class Box:
    pass


def listed(a):
    yield [*a, (yield) or 9]


def merged(m):
    yield {**m, "k": (yield) or 3}


def called(a, m):
    yield dict(*a, **m, k=(yield) or 4)


def counted(box):
    box.total += (yield) or 5
    yield box.total


def indexed(d):
    d["x"] += (yield) or 5
    yield d["x"]


def extended(items):
    items += (yield) or [2]
    yield items


a = [1]
g = listed(a)
next(g)
a.append(2)
print(next(g))
m = {"a": 1}
g = merged(m)
next(g)
m["b"] = 2
print(next(g))
pairs = [("p", 1)]
g = called([pairs], m)
next(g)
pairs.append(("q", 2))
m["c"] = 3
print(next(g))
box = Box()
box.total = 1
g = counted(box)
next(g)
box.total = 100
print(next(g), box.total)
d = {"x": 1}
g = indexed(d)
next(g)
d["x"] = 50
print(next(g), d)
items = [1]
g = extended(items)
next(g)
print(next(g), items)
