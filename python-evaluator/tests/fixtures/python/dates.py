"""Exercises datetime construction, parsing, arithmetic, and durations."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
from datetime import datetime, timedelta, timezone

start = datetime(2026, 9, 29, 12, 30)
print(start.year, start.month, start.day, start.hour, start.minute, start.isoformat())
later = start + timedelta(days=1, hours=2)
print(later.strftime("%Y-%m-%d %H:%M"), (later - start).total_seconds(), later > start)
print(str(timedelta(days=2, seconds=3661)), str(timedelta(minutes=90)), timedelta(hours=1) * 3 == timedelta(hours=3))
print((timedelta(days=1) - timedelta(hours=1)).days, (timedelta(days=1) - timedelta(hours=1)).seconds)
parsed = datetime.strptime("2026-01-05", "%Y-%m-%d")
print(parsed.year, parsed.day, parsed.strftime("%d.%m.%Y"))
print(isinstance(datetime.now(timezone.utc), datetime), isinstance(timezone.utc, timezone))
print(datetime(2026, 1, 1, tzinfo=timezone.utc).isoformat()[:19])
from datetime import datetime, timedelta, timezone

start = datetime.fromisoformat("2015-01-01T00:00:00Z")
naive = datetime.fromisoformat("2016-06-30 12:30:00")
print(start, start.tzinfo is None, naive, naive.tzinfo is None, start.isoformat())
aware = naive.replace(tzinfo=timezone.utc)
print(aware, aware.year, aware - start > timedelta(days=500), aware.astimezone(timezone.utc).hour)
print(datetime.fromisoformat("2020-02-03").weekday(), datetime(2020, 1, 1, tzinfo=timezone.utc).isoformat())
import time
print(time.strftime("%Y-%m-%d %H:%M", time.gmtime(86400 * 365 + 3600)), time.strftime("%Y", time.localtime(86400 * 400)))
print(time.asctime(time.gmtime(86400 * 3 + 5)), time.asctime(time.gmtime(86400 * 40)))
