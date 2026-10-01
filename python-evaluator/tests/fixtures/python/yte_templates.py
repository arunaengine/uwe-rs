"""Exercises YTE expressions, conditions, loops, definitions, variables, and merges."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
from yte import process_yaml, SKIP

template = """
__definitions__:
  - |
    def double(x):
        return 2 * x
__variables__:
  base: ?10 + 1
plain: 1
computed: ?double(base) + offset
?if offset > 5:
  big: yes
?elif offset > 1:
  middle: yes
?else:
  small: yes
items:
  - 1
  - ?SKIP
  - <: ?[7, 8]
  - ?f"x{offset}"
?for name, number in [("a", 1), ("b", 2)]:
  ?f"key_{name}": ?number * offset
merged:
  first: 1
  <: ?dict(second=2)
"""
print(process_yaml(template, variables={"offset": 3}))
print(process_yaml("a: ?1 + 1\n", require_use_yte=True), process_yaml("__use_yte__: true\na: ?1 + 1\n", require_use_yte=True))
