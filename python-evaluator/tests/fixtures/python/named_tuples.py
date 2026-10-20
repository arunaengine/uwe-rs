"""Exercises the argument and name checks of collections.namedtuple."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
from collections import namedtuple

Point = namedtuple("Point", "x y z", defaults=(0,))
print(Point(1, 2), Point(1, y=2, z=3))
calls = [
    lambda: Point(1, 2, 3, 4),
    lambda: Point(1, w=2),
    lambda: Point(),
    lambda: Point(1, x=2),
    lambda: namedtuple("Empty", "")(1),
    lambda: namedtuple("Triple", "a b c")(),
]
names = [
    ("Pair", "x x"),
    ("Pair", "x _y"),
    ("Pair", "x def"),
    ("Pair", "x 1y"),
    ("1Pair", "x"),
]
for call in calls:
    try:
        print(call())
    except TypeError as error:
        print("TypeError", error)
for typename, fields in names:
    try:
        namedtuple(typename, fields)
    except ValueError as error:
        print("ValueError", error)
try:
    namedtuple("Pair", "x", defaults=(1, 2))
except TypeError as error:
    print("TypeError", error)
print(namedtuple("Renamed", "x def _z x", rename=True)._fields)
