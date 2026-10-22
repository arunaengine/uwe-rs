"""Exercises generators that suspend inside try, except, else, finally and match."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
def guarded(items):
    for item in items:
        try:
            yield 10 / item
        except ZeroDivisionError as error:
            yield 'caught ' + str(error)
        else:
            yield 'fine'
        finally:
            yield 'cleanup'
    try:
        yield 'last'
    finally:
        print('closing')


print(list(guarded([2, 0])))


def rethrown():
    try:
        try:
            yield 1
            raise KeyError('inner')
        except KeyError:
            yield 'handled'
            raise
        finally:
            yield 'finally'
    except KeyError as error:
        yield 'outer ' + repr(error)
    yield 'error' in dir()


print(list(rethrown()))


def leaves():
    for item in range(3):
        try:
            if item == 1:
                continue
            if item == 2:
                break
            yield item
        finally:
            yield 'left %d' % item
    try:
        return
    finally:
        yield 'returned'


print(list(leaves()))


def chosen(values):
    for value in values:
        match value:
            case [first, *rest]:
                yield ('list', first, rest)
            case {'key': found} if found > 1:
                yield ('dict', found)
            case int() as number:
                yield ('int', number)
                yield ('again', number)
            case _:
                yield 'other'


print(list(chosen([[1, 2, 3], {'key': 2}, {'key': 0}, 5])))
