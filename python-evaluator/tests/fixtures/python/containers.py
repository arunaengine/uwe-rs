"""Exercises Python lists, tuples, dictionaries, and sets."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
items = [3, 1, 2]
items.append(5)
items.extend([9, 0])
items.insert(1, 7)
print(items, items.pop(), items.pop(0), items.index(2), items.count(1), len(items))
items.sort()
print(items, sorted(items, reverse=True), sorted(["b", "A", "c"], key=str.lower), list(reversed(items)))
items.remove(7)
del items[0]
print(items, items[1:], items[::2], items[-1], 5 in items, 6 not in items)
a, *rest = [1, 2, 3, 4]
x, (y, z) = 1, (2, 3)
first, *middle, last = "abcde"
print(a, rest, x, y, z, first, middle, last)
d = {"b": 2, "a": 1}
d["c"] = 3
d.update({"a": 10}, d=4)
print(d, list(d), list(d.keys()), list(d.values()), list(d.items()), d.get("z"), d.get("z", 0))
print(d.pop("b"), d.setdefault("e", 5), d.setdefault("a", 0), "a" in d, len(d))
del d["e"]
print(d, dict(zip("xy", [1, 2])), dict([("k", "v")]), dict(a=1), {**d, "q": 0}, d | {"a": 0})
s = {3, 1, 2}
s.add(4)
s.discard(10)
print(sorted(s), sorted(s | {9}), sorted(s & {1, 2}), sorted(s - {1}), sorted(s ^ {1, 5}), {1, 2} <= {1, 2, 3})
print(sorted({x * 2 for x in range(4)}), {k: v for k, v in zip("ab", "cd")}, [x for x in range(10) if x % 3 == 0])
print([(i, j) for i in range(3) for j in range(i)], list(map(str, [1, 2])), list(filter(None, [0, 1, "", "a"])))
print(list(enumerate("ab", 1)), list(zip([1, 2, 3], "ab")), any([0, 1]), all([]), max("abc"), min([3, 1], key=lambda v: -v))
print(tuple([1, 2]), list((1, 2)), set("aab") == {"a", "b"}, frozenset([1]) | {2}, list(range(2, 10, 3)), range(5)[-1])
nested = {"x": {"y": [1, {"z": "deep"}]}}
print(nested["x"]["y"][1]["z"], nested.get("x", {}).get("y"), [row[0] for row in [[1, 2], [3, 4]]])
matrix = [[0] * 2 for _ in range(2)]
matrix[0][1] = 5
print(matrix, [[1, 2], [3]] + [[4]], sum(len(r) for r in matrix))
counts = {}
for letter in "banana":
    counts[letter] = counts.get(letter, 0) + 1
print(counts, sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))
lst = list(range(10))
lst[2:5] = ["a"]
print(lst, lst[-3:], lst[:-7:-2])
print(sorted([(2, "b"), (1, "z"), (2, "a")]), sorted([[3], [1, 2]]), [1, 2, 3][::-1] == [3, 2, 1])
it = iter([1, 2, 3])
print(next(it), next(it), list(it), next(iter([]), "done"))
print({1: "a", True: "b"}, list(dict.fromkeys(["x", "y", "x"])), {(1, 2): "t"}[(1, 2)])
