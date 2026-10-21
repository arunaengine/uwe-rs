"""Checks time.strftime and time.asctime with plain tuples, and the C locale forms of dates."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import datetime
import time

moment = (2024, 7, 14, 17, 31, 5, 6, 196, 0)
print(time.strftime("%Y-%m-%d %H:%M:%S %a %j", moment))
print(time.asctime(moment), time.asctime((2024, 1, 1, 0, 0, 0, 0, 1, 0)))
print(time.strftime("%c|%x|%X|%%c", moment))
print(datetime.datetime(2024, 7, 4, 9, 5, 1).strftime("%c|%x|%X"))
for bad in [(2024, 1), "x"]:
    try:
        time.strftime("%Y", bad)
    except TypeError as error:
        print("TypeError", error)
given = (2024, 1, 1, 0, 0, 0, 3, 100, 0)
print(time.strftime("%a %A %w %u %j|%c", given), time.asctime(given))
try:
    time.strftime("%a", (2024, 1, 1, 0, 0, 0, 0, 400, 0))
except ValueError as error:
    print("ValueError", error)
