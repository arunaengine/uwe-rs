"""Checks keys that equal other kinds of keys by __eq__, and dataclass fields left out of __eq__ and __hash__."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
from dataclasses import dataclass, field


class Same:
    def __init__(self, value):
        self.value = value

    def __eq__(self, other):
        return self.value == (other.value if isinstance(other, Same) else other)

    def __hash__(self):
        return hash(self.value)


table = {"a": 1}
print(table[Same("a")], Same("a") in table, table.get(Same("a")), Same("a") in {"a"}, "a" in {Same("a")})
table[Same("a")] = 2
print(table, len(table), table.setdefault(Same("a"), 9), table.pop(Same("a")), table)
reverse = {Same("b"): 1}
print(reverse["b"], "b" in reverse, reverse.get("c"))
pairs = {(1, "x"): 3, 7: 4}
print(pairs[Same((1, "x"))], pairs[Same(7)], Same(7.0) in pairs)


@dataclass(frozen=True)
class Point:
    x: int
    note: str = field(default="", compare=False)
    tag: int = field(default=0, hash=False)


print(Point(1, "a") == Point(1, "b"), hash(Point(1, "a", 5)) == hash(Point(1, "b", 6)))
print(Point(1, tag=1) == Point(1, tag=2), len({Point(1, "a"), Point(1, "b")}), Point(2) == Point(1))


@dataclass
class Shown:
    x: int
    secret: str = field(default="s", repr=False)
    y: int = 2


print(Shown(1), repr(Shown(3, "k", 4)))
