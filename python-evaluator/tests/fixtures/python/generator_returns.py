"""Checks what generators return: StopIteration.value and x = yield from g."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


def inner(start):
    got = yield start
    if got is None:
        return "plain"
    return f"sent {got}"


def outer():
    first = yield from inner(1)
    second = yield from inner(2)
    third = yield from [7, 8]
    yield (first, second, third)
    return "done"


g = outer()
print(next(g), next(g), g.send("x"), next(g), next(g))
try:
    next(g)
except StopIteration as stop:
    print("returned", stop.value, stop.args)
g = inner(0)
next(g)
try:
    g.send(5)
except StopIteration as stop:
    print(stop.value)
print(StopIteration(3).value, StopIteration().value, next(iter([]), "empty"))

