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

# Expressions.
added = df.with_columns(pl.lit(7).alias("c"), (pl.col("a") > 1).alias("big"))
print(added.columns, added["c"].to_list(), added["big"].to_list())
print(df.with_columns(pl.col("a").cast(pl.String))["a"].to_list())
print(df.select("b", pl.col("a").alias("x")).to_dicts())
try:
    df.with_columns(pl.lit(-1).cast(pl.UInt8))
except Exception as e:
    print(type(e).__name__)

# Rows that expressions select, sorted rows and lazy tables.
print(df.filter(pl.col("a").eq(2) | pl.col("b").is_null())["a"].to_list())
print(df.filter(~pl.col("b").is_in(["x"]))["a"].to_list())
print(df.filter((pl.col("a") >= 2) & pl.col("b").is_not_null())["b"].to_list())
print(df.sort("a", descending=True)["a"].to_list())
print(df.sort(["b", "a"])["b"].to_list())
print(df.head(2).height, pl.concat([df, df]).height)
lazy = df.lazy().filter(pl.col("a") > 1)
print(isinstance(lazy, pl.LazyFrame), isinstance(df, pl.LazyFrame), lazy.collect()["a"].to_list())
print(pl.concat([lazy, lazy]).collect().height)
