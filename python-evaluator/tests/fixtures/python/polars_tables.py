# The polars stand-in: data types, series, tables from dicts and CSV files, expressions and
# lazy tables.
import polars as pl

# Data types and series.
s = pl.Series("a", [1, None, 3])
print(s.name, s.to_list(), s.dtype, len(s), s[-1], list(s), pl.Int64, pl.UInt8, pl.String)
print(isinstance(s, pl.Series), pl.Series([1.5, 2]).dtype)

# Tables from dicts.
df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", None]})
print(df.columns, df.shape, df.height, len(df), df.dtypes)
print(df[0, "a"], df[2, "b"], df[-1, "a"], df["b"].to_list(), df[1].to_dicts())
print(list(df.iter_rows(named=True)))
print(df.rows())
print(pl.DataFrame([{"x": 1}, {"x": 2, "y": "z"}]).to_dicts())
print(isinstance(df, pl.DataFrame))
try:
    df[0, "missing"]
except Exception as e:
    print(type(e).__name__)
