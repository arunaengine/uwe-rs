"""Exercises arithmetic with Python numeric rules."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
print(7 // 2, -7 // 2, 7 % 3, -7 % 3, 7 % -3, 7.5 // 2, -7.5 % 2)
print(2 ** 10, 2 ** 100, 2 ** -1, 10 / 4, 9 / 3, 1 / 3)
print(0.1 + 0.2, 1e16, 1.5e-5, 123456789.0, 1e22, -0.0, 3.0, 2.5e-10)
print(int("42"), int("-0x1f", 16), int("0b101", 0), int(3.99), int(-3.99), float("1e3"), float("inf"))
print(round(2.5), round(3.5), round(-2.5), round(2.675, 2), round(1234, -2), round(0.5))
print(abs(-3), abs(-2.5), divmod(17, 5), divmod(-17, 5), pow(2, 10, 1000), max(3, 7, 1), min([4, 2, 9]))
print(True + True, True * 3, 1 == 1.0, hash(1) == hash(1.0), 3 < 4.5, 10 ** 20 > 10 ** 19)
print(bin(10), hex(255), oct(8), int("1_000"), 1_000_000, 0x10, 0o17, 0b11)
print(sum([1, 2, 3]), sum([0.5, 0.25]), sum([[1], [2]], []))
print(float(1), 7 / 7, 2 ** 0.5, 10 ** 2, 12345678901234567890 * 98765432109876543210)
print(-2 ** 2, (-2) ** 2, 5 & 3, 5 | 3, 5 ^ 3, ~5, 1 << 70, 1024 >> 3)
print(1 if 0 else 2, not 0, not [], bool("x"), bool(0.0), None is None)
print(3 * "ab", [0] * 3, (1,) * 2, "a" < "b", [1, 2] < [1, 3], (1, 2) == (1, 2))
x = 5
x += 2
x *= 3
x -= 1
x //= 4
x **= 2
print(x)
print(format(3.14159, ".2f"), format(42, "05d"), format(1234567, ","), format(0.25, "%"), format(255, "x"))
print(format(1e-7, "g"), format(123456789, "e"), format(0.0001234, ".3g"), format(12.0, "g"), format(1.5, ".0f"))
z = complex(1, 2)
print(z, 2j, complex(0, -2), z * (1 - 1j), (z + 1) / 2, abs(3 + 4j), z.real, z.imag, z == 1 + 2j, isinstance(z, complex))
