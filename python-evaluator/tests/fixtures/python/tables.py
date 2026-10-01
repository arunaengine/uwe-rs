"""Exercises pandas sample-sheet input, indexing, and reshaping."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import io
import pandas as pd
import numpy as np

samples = pd.read_csv("pandas_samples.tsv", sep="\t", comment="#")
print(samples.columns.tolist(), samples.shape, len(samples))
print(samples.dtypes.to_dict() == {"sample": "str", "unit": "int64", "fq1": "str", "fq2": "str", "lane": "float64", "condition": "str"})
print(samples["sample"].tolist(), samples["lane"].tolist(), samples["fq2"].isna().tolist())
print(samples.to_dict("records")[0])

typed = pd.read_csv("pandas_samples.tsv", sep="\t", comment="#", dtype=str)
print(typed["sample"].tolist(), typed["unit"].tolist(), typed["lane"].tolist())
table = pd.read_table("pandas_samples.tsv", comment="#", dtype={"sample": str})
print(table["sample"].tolist(), table["unit"].tolist())

indexed = typed.set_index("sample", drop=False)
print(indexed.index.tolist(), indexed.index.name, indexed.columns.tolist())
print(indexed.loc["A", "fq1"], indexed.loc["A"]["condition"])
print(indexed.loc["B", "fq1"].tolist(), indexed.loc["B", "fq1"].index.tolist())
print(indexed.loc[["A", "01"], "condition"].tolist())
print("A" in indexed.index, "Z" in indexed.index, "fq1" in indexed)
try:
    indexed.loc["Z"]
except KeyError as error:
    print("KeyError", error)

units = typed.set_index(["sample", "unit"], drop=False)
units.index.names = ["sample_id", "unit_id"]
print(units.index.names, units.index.nlevels)
print(units.loc[("B", "2"), "fq1"], units.loc[("B", "2"), ["fq1", "fq2"]].dropna().tolist())
print(units.loc["B"].index.tolist(), units.loc["B", "fq1"].tolist())
print([level.tolist() for level in units.index.levels])
units.index = units.index.set_levels([level.astype(str) for level in units.index.levels])
print(units.index.tolist())
print(units.loc[("A", "1")].tolist())

print(samples[samples["condition"] == "control"]["fq1"].tolist())
print(samples.loc[samples["unit"] > 1, "fq1"].tolist())
print(samples[(samples.condition == "treated") & (samples.lane > 1)].fq1.tolist())
print(samples[~samples["fq2"].isna()]["sample"].unique().tolist())
print(samples["sample"].isin(["A", "B"]).tolist(), samples["condition"].nunique())
print(samples.query("condition == 'control' and unit == 2")["fq1"].tolist())
print(samples.query("sample in ['A', '01']")["fq1"].tolist())

for row in samples.head(2).itertuples():
    print(row.Index, row.sample, row.fq1, row.lane)
for label, row in samples.tail(1).iterrows():
    print(label, row["fq1"], row.condition)

print(samples["lane"].astype(str).tolist(), samples["unit"].astype(str).tolist())
print(samples.fillna({"fq2": "none"})["fq2"].tolist(), samples["fq2"].fillna("").tolist())
print(samples.dropna()["sample"].tolist(), samples.dropna(subset=["lane"])["sample"].tolist())
print(samples["fq1"].str.replace(".fq", ".fastq").tolist(), samples["fq1"].str.startswith("b").tolist())
print(samples["condition"].str.upper().tolist(), samples["fq1"].str.split(".").str[0].tolist())
print(samples["sample"].str.contains("0").tolist())
print(samples.drop_duplicates(subset=["sample"])["unit"].tolist(), samples["sample"].duplicated().tolist())
print(samples.sort_values("fq1", ascending=False)["fq1"].tolist(), samples.sort_values(["sample", "unit"])["fq1"].tolist())
print(samples.groupby("sample").size().to_dict())
print({key: group["fq1"].tolist() for key, group in samples.groupby("sample")})
print(samples.groupby("condition")["fq1"].apply(list).to_dict())
print(samples.groupby("condition").first()["fq1"].tolist())

frame = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
print(frame.to_dict(), frame.to_dict("list"), frame.to_dict("index"))
frame["c"] = frame["a"] * 2
frame.loc[1, "b"] = "Y"
print(frame.values.tolist(), [str(value) for value in frame.iloc[0]], frame.iloc[-1, 1], frame.at[2, "c"], frame.iat[0, 1])
rows = pd.DataFrame([{"x": 1, "y": "p"}, {"x": 2}])
print(rows.columns.tolist(), rows["y"].tolist(), pd.isna(rows.loc[1, "y"]))
print(pd.concat([frame, frame], ignore_index=True)["a"].tolist())
left = pd.DataFrame({"key": ["k1", "k2", "k3"], "l": [1, 2, 3]})
right = pd.DataFrame({"key": ["k1", "k3", "k4"], "r": ["a", "c", "d"]})
print(pd.merge(left, right, on="key").values.tolist())
print(left.merge(right, on="key", how="left")["r"].tolist())
series = pd.Series([3, 1, 2], index=["c", "a", "b"], name="n")
print(series["a"], series.sort_index().tolist(), series.sort_values().index.tolist(), series.to_dict())
print(series.value_counts().to_dict(), series.max(), series.sum(), series.mean())
print(pd.Series({"x": 1, "y": 2}).index.tolist(), pd.Series(5, index=["p", "q"]).tolist())
print(pd.isna(np.nan), pd.notna("x"), pd.isna(None), np.isnan(float("nan")))
print(np.unique(["b", "a", "b"]).tolist(), np.array([1, 2]).tolist(), np.int64("7"))
text = io.StringIO("id,value\nx,1\ny,2\n")
print(pd.read_csv(text, index_col="id").loc["y", "value"])
reset = indexed.reset_index(drop=True)
print(reset.index.tolist(), indexed.rename(columns={"fq1": "read1"}).columns.tolist()[:3])
try:
    indexed.reset_index()
except ValueError as error:
    print("ValueError", error)
print(pd.DataFrame({"s": ["a", "b"]}).set_index("s").reset_index().columns.tolist())
print(indexed.index.unique().tolist(), list(indexed.index), indexed.index.is_unique)
print(samples.apply(lambda row: row["fq1"] + ":" + str(row["unit"]), axis=1).tolist())
print(samples["unit"].map({1: "one", 2: "two"}).tolist(), samples["unit"].apply(lambda value: value * 10).tolist())
print(samples.empty, pd.DataFrame().empty, samples.get("missing", "default"), list(samples)[:2])
print(samples.query("condition == 'control' & unit == 2")["fq1"].tolist(), samples.query("unit == 2 | sample == 'A'")["fq1"].tolist())
print(pd.Series(["a,b", "c", ""]).str.split(",").explode().tolist(), samples.astype(dtype={"unit": str})["unit"].tolist())
gaps = pd.DataFrame({"name": ["a", None], "size": [1.5, np.nan]})
print(gaps.where(gaps.notnull(), None).to_dict(orient="records"), gaps.replace(np.nan, "NA").to_dict(orient="records"))
print(gaps.replace({np.nan: None})["size"].tolist(), gaps.mask(gaps.isnull(), "x").to_dict(orient="list"))
print(pd.DataFrame({"group": ["a", "a", "b"], "sample_id": ["x", "y", "z"]}).groupby("group").sample_id.apply(list).to_dict())
print(pd.Series(["a", None, "b"]).str.cat(pd.Series(["c", "d", "e"]), sep="_").tolist(), pd.Series(["a", None]).str.cat(sep="-", na_rep="?"))
df = pd.DataFrame({"sample": ["A", "A", "B"], "rep": [1, 2, 1], "fq": ["x", None, "z"]}).set_index(["sample", "rep"], drop=False)
print(df.loc[["A"]].fq.tolist(), df.loc[["A", "B"]].shape, list(df.index.get_level_values("sample").unique()))
print(all(df.loc[["B"]].fq.notna()), df.loc["A"].shape)
print("H3" in pd.DataFrame({"a": ["H3", "x"]})[["a"]].values, "y" in pd.DataFrame({"a": ["H3", "x"]})[["a"]].values)
try:
    pd.read_table(io.StringIO("a b\n1 2\n"), delim_whitespace=True)
except TypeError as error:
    print("TypeError", error)
grouped = pd.DataFrame({"s": ["a", "a", "b"], "v": [1, 2, 5]}).groupby("s")
print(grouped.s.transform("count").tolist(), grouped["v"].transform("sum").tolist(), grouped.v.transform(lambda x: x - x.min()).tolist())
print(pd.DataFrame({"a": ["x", "y"], "b": [1, 2], "c": [3, 4]}).set_index(["a", "b"]).droplevel("b").index.tolist())
wide = pd.DataFrame({"sample_id": ["a", "b"], "unit": [1, 1], "fq1": ["a1", "b1"], "fq2": ["a2", None]})
long = pd.wide_to_long(wide, "fq", i=["sample_id", "unit"], j="read")
print(long.index.tolist(), long["fq"].tolist(), long.dropna().reset_index()[["sample_id", "read"]].values.tolist())
print(pd.DataFrame({"x": ["p", "q"]}).squeeze("columns").tolist(), pd.DataFrame({"x": [3]}).squeeze())
try:
    pd.read_csv(io.StringIO(""), header=None)
except pd.errors.EmptyDataError as error:
    print("EmptyDataError", error)
