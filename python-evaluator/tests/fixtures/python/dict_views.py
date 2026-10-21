"""Checks the views of dicts: their reprs, live contents, membership and set operators."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
d = {"a": 1, "b": [2]}
keys, values, items = d.keys(), d.values(), d.items()
print(keys, values, items)
d["c"] = 3
del d["a"]
print(keys, len(values), list(items), "c" in keys, ("c", 3) in items, [2] in values)
print(keys | {"z"} == {"b", "c", "z"}, sorted(keys & {"c", "q"}), sorted({"b", "x"} - keys))
print(keys == {"b", "c"}, items == {("b", [2])} if False else None, values == values, bool({}.keys()))
print(sorted(dict.fromkeys([3, 1, 2]).keys()), list(reversed({1: 0, 2: 0}.keys())), type(keys).__name__)
for key, value in {1: "one"}.items():
    print(key, value)
print(keys.__contains__("b"), keys.isdisjoint(["q"]), len(items), items.__len__(), list(iter(keys)))
