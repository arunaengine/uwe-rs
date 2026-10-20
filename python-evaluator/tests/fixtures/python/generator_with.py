"""Exercises generators that yield inside with statements."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


class Manager:
    def __init__(self, name, suppress=False):
        self.name = name
        self.suppress = suppress

    def __enter__(self):
        print("enter", self.name)
        return self.name

    def __exit__(self, kind, value, trace):
        print("exit", self.name, kind.__name__ if kind else None)
        return self.suppress


def lines():
    with Manager("a") as name, Manager("b"):
        for number in range(2):
            yield f"{name}{number}"
    yield "after"


def failing():
    with Manager("c", suppress=True):
        yield 1
        raise ValueError("inside")
    yield 2


print(list(lines()))
print(list(failing()))
