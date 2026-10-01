"""Exercises string methods, formatting, and f-strings."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
s = "  Hello, World  "
print(repr(s.strip()), repr(s.lstrip()), repr(s.rstrip()), s.lower(), s.upper())
print("a,b,,c".split(","), "a b  c".split(), "a,b,c".split(",", 1), "a,b,c".rsplit(",", 1))
print("x".join(["1", "2", "3"]), "-".join("abc"), "hello".replace("l", "L"), "hello".replace("l", "L", 1))
print("hello".startswith("he"), "hello".endswith(("lo", "x")), "hello".find("l"), "hello".rfind("l"), "hello".find("z"))
print("hello".count("l"), "Hello World".title(), "hello world".capitalize(), "HeLLo".swapcase())
print("abc".center(9, "*"), "abc".ljust(6, "."), "abc".rjust(6), "42".zfill(5), "-42".zfill(5))
print("a1".isalnum(), "abc".isalpha(), "123".isdigit(), "  ".isspace(), "Abc".istitle(), "abc".islower())
print("file.tar.gz".partition("."), "file.tar.gz".rpartition("."), "prefix_x".removeprefix("prefix_"), "x.txt".removesuffix(".txt"))
print("line1\nline2\r\nline3".splitlines(), "a\tb".expandtabs(4), "abc"[::-1], "abcdef"[1:4], "abcdef"[-2:])
name, number = "sample", 3.14159
print(f"{name}: {number:.2f} {number!r} {name!r:>10} {{literal}} {number:8.3f}|")
print(f"{'nested ' + name}", f"{1 + 2 = }", f"{name:^12}", f"{42:#x}", f"{1000000:_}")
print("{} and {}".format("a", "b"), "{1} {0}".format("a", "b"), "{x}-{y}".format(x=1, y=2))
print("{0[0]} {0[1]}".format(["p", "q"]), "{a[key]}".format(a={"key": "v"}), "{:>5}|{:<5}|{:^5}|".format(1, 2, 3))
print("%s is %d years, %.2f%%" % ("Bob", 30, 99.5), "%5s|%-5s|" % ("a", "b"), "%(x)s-%(y)03d" % {"x": "k", "y": 7})
print("%r %x %o %e %g" % ("q", 255, 8, 12345.678, 0.00001234))
print(str(1.0), str(None), str([1, "a", None]), repr("it's"), repr('say "hi"'), str({"k": (1, 2)}))
print(len("héllo"), "héllo"[1], "héllo".upper(), ord("é"), chr(233), "ß".lower())
print("a-b_c d".translate(str.maketrans("-_", "  ")), "abc".translate({ord("a"): None}))
t = "{sample}/{unit}.bam"
print(t.format(sample="A", unit=1), "x{0}y".format(5))
print("%.3s|" % "abcdef", "%+d %+d" % (5, -5), "% d" % 42, "%c" % 65, "%i" % 3.7)
print(str(12345678901234567890), repr(1e100), repr(float("nan")), repr(-float("inf")))
print("{:,.2f}".format(1234567.891), "{:e}".format(0.0), "{:.1%}".format(0.123), "{:g}".format(100000000.0))
