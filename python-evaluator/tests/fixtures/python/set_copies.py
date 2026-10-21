"""Checks the order of sets that set operations copy, merge, change and pop."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
print(list({16, 32, 1, 0, 48} | {64}))
s = {16, 32, 1, 0, 48}
print(s.pop(), s.pop(), list(s))
t = {0, 8}
t.discard(0)
t.add(16)
print(list(t))
a = set([48, 128, 0, (0, 8), 3.5, 32, -1, -3, 16]); b = set([48, (0, 8), 0, 16, 0.25, 1, -3, 255]); c = [-1, 1, 7, 64, 56]
print(a.pop() if a else None)
print(list(a))
a = a.difference(b, c)
print(list(a))
a.symmetric_difference_update(b)
print(list(a))
a = a - b
print(list(a))
a = a & b
print(list(a))
a.difference_update(c)
print(list(a))
a = set([1099511627776, 1]); b = set([24, 9, 40, 2305843009213693952, 3.5, 0, 128, 127, 0.25, (0, 8), 100, 7]); c = [7, 31]
a.update(b, c)
print(list(a))
a = a | b
print(list(a))
a = a ^ b
print(list(a))
a = set(a)
print(list(a))
a = a | b
print(list(a))
a = set(a)
print(list(a))
a = set([-1, 0, 9, 32]); b = set([-3, (0, 8), 24, -1, 2305843009213693952, 255, 1099511627776, 0, 2]); c = [24, 100, 16, 2305843009213693952, 255, 32]
a = a.intersection(b, c)
print(list(a))
a.clear()
print(list(a))
a.discard(100)
print(list(a))
a = a.intersection(b, c)
print(list(a))
print(a.pop() if a else None)
print(list(a))
a = a.copy()
print(list(a))
a = set([255, 1099511627776]); b = set([15, 31, -3, 16, (0, 8), 1, 0, 127, 64]); c = [9, 8]
a = a & b
print(list(a))
a &= b
print(list(a))
a.add(100)
a.add(40)
a.add(9)
a.add(127)
print(list(a))
a = a.difference(b, c)
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a = a.union(b, c)
print(list(a))
a = set([100, 40, 1099511627776, 64, 1, 8, 48, 16, 24, 31]); b = set([40, 64, -1]); c = [2, 24, 64, -1, 32, 9]
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a = a.symmetric_difference(c)
print(list(a))
a = a ^ b
print(list(a))
a.symmetric_difference_update(b)
print(list(a))
a.difference_update(c)
print(list(a))
a &= b
print(list(a))
a = set([0.25]); b = set([3.5, (0, 8), -1, 8, -3, 1099511627776, 0.25, 15, 7]); c = [1, 100, 2]
a.clear()
print(list(a))
a ^= b
print(list(a))
a = a | b
print(list(a))
a.clear()
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a.intersection_update(b)
print(list(a))
a = set([128, 48, 32, 100, 24, 0.25]); b = set([0.25, 7, 1, 24, (0, 8), 127]); c = [15, 72, 2305843009213693952, 31]
a.intersection_update(b)
print(list(a))
a.add(32)
print(list(a))
a.intersection_update(b)
print(list(a))
a = set(a)
print(list(a))
print(a.pop() if a else None)
print(list(a))
a = a & b
print(list(a))
a = set([0.25]); b = set([127, 9, 7]); c = [0.25, 24, 56, 72, 9]
a.clear()
print(list(a))
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a.add(9)
print(list(a))
a.difference_update(c)
print(list(a))
a.update(b, c)
print(list(a))
a = set([8, 64, 72, 16]); b = set([128, (1, 2), 32, 7, -1, 8, 127, 56, 0.25, 2]); c = [64, 16, -1, 48, 40, 56]
a.intersection_update(b)
print(list(a))
a = a & b
print(list(a))
a.clear()
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a -= b
print(list(a))
a = a.union(b, c)
print(list(a))
a = set([(0, 8), 255, 7, 31, 0.25, 8, -3, 1, 48, 127, 24]); b = set([127, (1, 2), 128, 2, 15]); c = [(0, 8), 100, 16, -3, 0.25]
a &= b
print(list(a))
a ^= b
print(list(a))
a.add((0, 8))
print(list(a))
a = a | b
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = set([40, 7, 128, 64, 1099511627776, 24, 0.25]); b = set([100, -3, 128, 7, 16, 56]); c = [2305843009213693952, 31]
a = a.intersection(b, c)
print(list(a))
a -= b
print(list(a))
a = a ^ b
print(list(a))
a.update(b, c)
print(list(a))
a.symmetric_difference_update(b)
print(list(a))
a.intersection_update(b)
print(list(a))
a = set([2305843009213693952, -1, 24, 100, 48, 9, 0.25, 128, 56, 2, (1, 2)]); b = set([7, 9, -3, 32, 128]); c = [127]
a = a.copy()
print(list(a))
a = a.symmetric_difference(c)
print(list(a))
a.update(b, c)
print(list(a))
a = set(a)
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a = set([24, 0.25, 0]); b = set([16, 8, 255, 128, 0, 127, -1, 9, 7, 1099511627776, 2]); c = [32]
a ^= b
print(list(a))
a -= b
print(list(a))
a = a.difference(b, c)
print(list(a))
a = a.copy()
print(list(a))
a = a | b
print(list(a))
a = a & b
print(list(a))
a = set([1099511627776, 9, 56, 128, 72, -1]); b = set([9, 3.5, 15, 40, 24, 48, 8, 255]); c = [48, 24, 127, (1, 2), (0, 8)]
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a ^= b
print(list(a))
a = a ^ b
print(list(a))
a &= b
print(list(a))
print(a.pop() if a else None)
print(list(a))
a.add(72)
a.add(2)
a.add(-1)
a.add(2305843009213693952)
print(list(a))
a = set([9, 0, -3, 48, (0, 8), 3.5, 7]); b = set([7, 9, 2, -1, (0, 8), 31, 24, 0]); c = [48, 16, 1, -3, 0.25, 72]
a = a & b
print(list(a))
a = a.intersection(b, c)
print(list(a))
a.add(1)
print(list(a))
a.intersection_update(b)
print(list(a))
a = a - b
print(list(a))
a |= b
print(list(a))
a = set([16, 64, 127, 40, 2305843009213693952]); b = set([127, -3, (0, 8), 64, 0.25, 24, 1099511627776, 72, 255, 100, 2, 1]); c = [16, (1, 2), 8]
print(a.pop() if a else None)
print(list(a))
a &= b
print(list(a))
a.difference_update(c)
print(list(a))
a &= b
print(list(a))
a -= b
print(list(a))
a = a & b
print(list(a))
a = set([9, 255, 0, 128, 127, 1, 48, -1, 72, 56, 24]); b = set([100, 31, 1099511627776, 7, (0, 8), 9, 24, 56, 8]); c = [64, 72]
print(a.pop() if a else None)
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = a.difference(b, c)
print(list(a))
a.discard(15)
print(list(a))
a = a ^ b
print(list(a))
a ^= b
print(list(a))
a = set([255]); b = set([31, 48, 72, 8, 2, 7, 1099511627776]); c = [72]
print(a.pop() if a else None)
print(list(a))
a.update(b, c)
print(list(a))
a.discard(128)
print(list(a))
a.add(1)
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = a.difference(b, c)
print(list(a))
a = set([3.5, 72, 24, 48, 15, 2305843009213693952, 8, 64, 40, 0, 100, 56]); b = set([(1, 2), 100, 32]); c = [9, 127, -3, 0, 40, 64]
a |= b
print(list(a))
a = a.copy()
print(list(a))
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a ^= b
print(list(a))
a = a.intersection(b, c)
print(list(a))
a &= b
print(list(a))
a = set([32, 0.25, 64, 1099511627776]); b = set([64, 2305843009213693952, 56, (1, 2), 7, 2, 1099511627776, 32, (0, 8), 9, 40]); c = [(1, 2), 7]
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a &= b
print(list(a))
a = set(frozenset(a) | frozenset(b))
print(list(a))
a &= b
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = a & b
print(list(a))
a = set([(1, 2), 48, 0, 100]); b = set([56, 0, 1099511627776, 48]); c = [-3, 3.5]
a = a.difference(b, c)
print(list(a))
a ^= b
print(list(a))
a.add(255)
print(list(a))
a = set(a)
print(list(a))
a.add(255)
a.add(64)
a.add(0)
a.add(128)
print(list(a))
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a = set([-1, 31, 16]); b = set([0, 0.25, 7, 128, 16, 48, 3.5, 24]); c = [15, 2, 128, 31, 48]
print(a.pop() if a else None)
print(list(a))
a = a.union(b, c)
print(list(a))
a -= b
print(list(a))
a.symmetric_difference_update(b)
print(list(a))
a.intersection_update(b)
print(list(a))
a = a.intersection(b, c)
print(list(a))
a = set([128, 100, 2305843009213693952, 72, -3]); b = set([100, 8, (0, 8)]); c = [2, 127, 56]
a = a.symmetric_difference(c)
print(list(a))
a.symmetric_difference_update(b)
print(list(a))
a = set(dict.fromkeys(list(a) + c))
print(list(a))
a = a.copy()
print(list(a))
a = a.intersection(b, c)
print(list(a))
a.add(7)
a.add(31)
a.add(9)
a.add(40)
print(list(a))
a = set([2305843009213693952, 0, -3, 100, 56, 1, 0.25]); b = set([(1, 2), 100, 31, 48, -1, 3.5, -3, 2, 8, 128, 24, 1]); c = [128, 2, 7, 24]
a.add(8)
print(list(a))
a = a.union(b, c)
print(list(a))
a = set(a)
print(list(a))
a = a ^ b
print(list(a))
print(a.pop() if a else None)
print(list(a))
a.discard(15)
print(list(a))
