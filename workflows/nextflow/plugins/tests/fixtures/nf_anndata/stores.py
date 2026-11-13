"""Writes the Zarr v2 and v3 stores of the nf-anndata tests with the zarr library."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import sys

import numcodecs
import numpy
import zarr


def store(path, version):
    root = zarr.open_group(path, mode="w", zarr_format=version)
    root.attrs.update({"encoding-type": "anndata", "encoding-version": "0.1.0"})
    if version == 2:
        codec = numcodecs.Blosc(cname="lz4", clevel=5, shuffle=1)
    else:
        codec = zarr.codecs.ZstdCodec(level=0)

    def array(group, name, values, dtype):
        data = numpy.asarray(values, dtype=dtype)
        node = group.create_array(name, shape=data.shape, chunks=(200,), dtype=dtype, compressors=[codec])
        node[:] = data

    def texts(group, name, values):
        node = group.create_array(name, shape=(len(values),), chunks=(200,), dtype=str, compressors=[codec])
        node[:] = numpy.array(values, dtype=object)

    obs = root.create_group("obs")
    obs.attrs.update({"_index": "cell_id", "column-order": ["kind", "flag", "small", "score", "count"]})
    cells = range(300)
    texts(obs, "cell_id", [f"c{i}" for i in cells])
    kind = obs.create_group("kind")
    texts(kind, "categories", ["A", "B"])
    array(kind, "codes", [i % 3 - 1 for i in cells], "int8")
    array(obs, "flag", [i % 2 == 0 for i in cells], "bool")
    array(obs, "small", [i * 7 % 256 for i in cells], "uint8")
    array(obs, "score", [i * 0.25 for i in cells], "float32")
    count = obs.create_group("count")
    array(count, "values", [i * 10 for i in cells], "int64")
    array(count, "mask", [i % 5 == 0 for i in cells], "bool")
    var = root.create_group("var")
    var.attrs.update({"_index": "_index", "column-order": []})
    texts(var, "_index", ["g1", "g2"])
    for name in ["layers", "obsm", "varm", "obsp", "varp", "uns"]:
        root.create_group(name)
    array(root["layers"], "counts", [1, 2, 3], "int32")
    root["uns"].create_group("neighbors")


store(sys.argv[1] + "/v2.zarr", 2)
store(sys.argv[1] + "/v3.zarr", 3)
