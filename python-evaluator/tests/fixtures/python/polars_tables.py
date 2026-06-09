# The polars stand-in: data types, series, tables from dicts and CSV files, expressions and
# lazy tables.
import polars as pl

# Data types and series.
s = pl.Series("a", [1, None, 3])
print(s.name, s.to_list(), s.dtype, len(s), s[-1], list(s), pl.Int64, pl.UInt8, pl.String)
print(isinstance(s, pl.Series), pl.Series([1.5, 2]).dtype)
