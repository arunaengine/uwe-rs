"""Checks send, throw and close of generators, also through yield from."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


def echo():
    total = 0
    while True:
        value = yield total
        if value is None:
            break
        total += value


g = echo()
print(next(g), g.send(5), g.send(7))
try:
    g.send(None)
except StopIteration:
    print("stopped")
try:
    echo().send(1)
except TypeError as error:
    print("TypeError", error)


def guarded():
    while True:
        try:
            yield 1
        except ValueError as error:
            yield f"caught {error}"


g = guarded()
next(g)
print(g.throw(ValueError("x")), next(g), g.throw(ValueError, "y"))
try:
    g.throw(KeyError("k"))
except KeyError as error:
    print("KeyError", error)
print(list(g))


def cleanup():
    try:
        yield 1
        yield 2
    finally:
        print("cleaned")


g = cleanup()
next(g)
print(g.close(), list(g))
cleanup().close()


def stubborn():
    while True:
        try:
            yield 1
        except GeneratorExit:
            yield 2


g = stubborn()
next(g)
try:
    g.close()
except RuntimeError as error:
    print("RuntimeError", error)


def inner():
    x = yield "a"
    print("inner got", x)
    y = yield "b"
    print("inner got", y)


def outer():
    yield from inner()
    yield "end"


g = outer()
print(next(g), g.send(1), g.send(2))
g = outer()
next(g)
try:
    g.throw(IndexError("deep"))
except IndexError as error:
    print("IndexError", error)
print(hasattr(iter([1]), "send"), hasattr(g, "send"), [1, 2] == [1, 2])
