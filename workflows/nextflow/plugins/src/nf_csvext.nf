/*
 * The csv_create operator of nf-csvext 0.2.0, which takes the channel first.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

def csv_create(source) {
    return csv_create(source, [:], null)
}

def csv_create(source, Closure closure) {
    return csv_create(source, [:], closure)
}

// One text line per item, after a line of the headers when there are headers.
def csv_create(source, Map options, Closure closure = null) {
    def headers = options['headers'] ?: []
    def sep = options['sep'] ?: ','
    def lines = source.map { item -> csvLine(closure != null ? closure.call(item) : item, headers, sep) }
    return headers ? channel.of(headers.join(sep)).concat(lines) : lines
}

// A list joined, a map's values of the headers with false-like values empty, else the text.
def csvLine(item, headers, sep) {
    if (item instanceof List) {
        return item.join(sep)
    }
    if (item instanceof Map) {
        return headers.collect { key -> item[key] ?: '' }.join(sep)
    }
    return item.toString()
}
