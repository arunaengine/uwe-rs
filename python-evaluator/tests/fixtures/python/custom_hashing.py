"""Exercises dict keys and set members whose classes define __eq__ and __hash__."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
from dataclasses import dataclass
from enum import IntEnum


@dataclass(frozen=True)
class Sample:
    name: str
    unit: int = 1


class Folded:
    def __init__(self, text):
        self.text = text

    def __eq__(self, other):
        return isinstance(other, Folded) and self.text.lower() == other.text.lower()

    def __hash__(self):
        return hash(self.text.lower())


class OnlyEq:
    def __eq__(self, other):
        return True


@dataclass
class Mutable:
    name: str


class Level(IntEnum):
    LOW = 1
    HIGH = 2


print(len({Sample("a"), Sample("a"), Sample("b")}), Sample("a") in {Sample("a")})
table = {Sample("a", 2): "first"}
table[Sample("a", 2)] = "second"
print(len(table), table[Sample("a", 2)], hash(Sample("a")) == hash(("a", 1)))
print(len({Folded("A"), Folded("a"), Folded("B")}), {Folded("x"): 1}.get(Folded("X")))
print(len({(Sample("a"), 1), (Sample("a"), 1)}), len(set([Folded("q"), Folded("Q")])))
print(dict.fromkeys([Sample("z"), Sample("z")]).keys().__len__(), hash(Folded("A")) == hash("a"))
print(len({Level.LOW, Level.LOW, Level.HIGH}), {Level.HIGH: "h"}[Level.HIGH])
for value in [OnlyEq(), Mutable("m")]:
    try:
        {value}
    except TypeError as error:
        print("TypeError", error)
