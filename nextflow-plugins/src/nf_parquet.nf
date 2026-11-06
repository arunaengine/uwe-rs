/*
 * The operators of nf-parquet 0.2.3, which take the channel first.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { parquetCheck; parquetItems; parquetWriter } from 'plugin/nf-parquet'

// Each row of each file as a map, or lists of `by` rows; `each` changes every item.
def splitParquet(source, Map options = [:]) {
    parquetCheck(options)
    def each = options['each']
    def items = source.flatMap { item -> parquetItems(item, options) }
    return each instanceof Closure ? items.map { item -> each.call(item) } : items
}

// The number of rows, or of `by` chunks, of all files together.
def countParquet(source, Map options = [:]) {
    parquetCheck(options)
    return source
        .map { item -> parquetItems(item, options).size() }
        .reduce(0) { total, count -> total + count }
}

// Checks the options as the plugin does; uwe then refuses, as it cannot load record classes.
def toParquet(source, path, Map options = [:]) {
    parquetWriter(path, options)
    return source
}
