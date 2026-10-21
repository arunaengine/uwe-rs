"""Checks sets and dicts whose keys of other kinds are equal by __eq__, and their reprs."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


class Same:
    def __init__(self, value):
        self.value = value

    def __eq__(self, other):
        return self.value == (other.value if isinstance(other, Same) else other)

    def __hash__(self):
        return hash(self.value)

    def __repr__(self):
        return f"Same({self.value!r})"


print({1, 2} - {Same(1)}, {Same(1)} | {1}, {1, 2} & {Same(2)})
print({1: "a", Same(1): "b"}, dict.fromkeys([1, Same(1)]), {Same(8), 8, 6})
print(set([Same(3), 3]), {k: 0 for k in [Same(4), 4]}, {v for v in [5, Same(5)]})
s = {1, 2}
s.update([Same(2), Same(9)])
print(len(s), Same(9) in s, s - {9}, frozenset([Same(7)]))
