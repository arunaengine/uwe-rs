# The datetime stand-in: construction, parsing, arithmetic and durations.
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
