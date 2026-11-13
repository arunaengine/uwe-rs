/*
 * The record helpers of the nf-bactopia plugin 2.1.7, which take a list or a channel and give the same.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { bactopiaGather; bactopiaFilter; bactopiaLogs; bactopiaCombine; bactopiaFormat } from 'plugin/nf-bactopia'

// The plugin's errors reach scripts once wrapped, as from a direct call of the plugin.
def unwrapped(Closure action) {
    try {
        return action.call()
    }
    catch (java.lang.reflect.InvocationTargetException e) {
        throw e.cause
    }
}

def gatherInto(results, mapping, meta) {
    if (results instanceof Collection || results == null) {
        def gathered = unwrapped { bactopiaGather(results, mapping, meta) }
        return gathered == null ? [] : gathered
    }
    unwrapped { bactopiaGather([], mapping, meta) }
    return results.toList().flatMap { items ->
        def gathered = bactopiaGather(items, mapping, meta)
        gathered == null ? [] : [gathered]
    }
}

def gather(results, field, meta) {
    return gatherInto(results, [(field): field], meta)
}

def gatherCsvtk(results, field, meta) {
    return gatherInto(results, [(field): 'csv'], meta)
}

def gatherFields(results, mapping, meta) {
    return gatherInto(results, mapping, meta)
}

def filterWithData(input, fields) {
    if (input instanceof Collection || input == null) {
        return unwrapped { bactopiaFilter(input, fields) }
    }
    return input.flatMap { item -> bactopiaFilter([item], fields) }
}

def collectNextflowLogs(results) {
    if (results instanceof Collection || results == null) {
        return unwrapped { bactopiaLogs(results) }
    }
    return results.flatMap { item -> bactopiaLogs([item]) }
}

def combineWith(gathered, items, field) {
    if (gathered instanceof Collection || gathered == null) {
        return unwrapped { bactopiaCombine(gathered, items, field) }
    }
    return gathered.combine(items.map { item -> [item] }).flatMap { pair -> bactopiaCombine([pair[0]], [pair[1]], field) }
}

def formatSamples(samples, dataTypes) {
    if (samples instanceof Collection || samples == null) {
        return unwrapped { bactopiaFormat(samples, dataTypes) }
    }
    unwrapped { bactopiaFormat([], dataTypes) }
    return samples.flatMap { sample -> bactopiaFormat([sample], dataTypes) }
}
