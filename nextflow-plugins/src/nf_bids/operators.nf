/*
 * The key operators of the nf-bids plugin 0.3.0, which take the channel first.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { bidsKey; bidsGroups; bidsPairs } from 'plugin/nf-bids'

def groupTupleBy(source, Closure key, Map options = [:]) {
    def size = options.size ?: 0
    def remainder = options.remainder != false
    def keyed = source.map { item -> [bidsKey(key.call(item)), item] }
    // With a size, groupTuple sends a full group at once instead of at the end of the source.
    def groups = size > 0
        ? keyed.groupTuple(size: size, remainder: remainder)
        : keyed.toList().flatMap { pairs -> bidsGroups(pairs, size, remainder) }
    return groups.map { group -> [group[0], sortedItems(group[1], options.sort)] }
}

// The items of a group sorted as `sort` asks: naturally, by a closure, or by a comparator.
def sortedItems(items, spec) {
    if (spec == null || spec == false) {
        return items
    }
    if (spec == true) {
        return items.sort(false)
    }
    return items.sort(false, spec)
}

def joinBy(left, right, Closure key) {
    return pairUp(left, right, key, key, false)
}

def joinBy(left, right, Closure key, Map options) {
    return pairUp(left, right, key, key, options.remainder == true)
}

def joinBy(left, right, Closure leftKey, Closure rightKey) {
    return pairUp(left, right, leftKey, rightKey, false)
}

def joinBy(left, right, Closure leftKey, Closure rightKey, Map options) {
    return pairUp(left, right, leftKey, rightKey, options.remainder == true)
}

def combineBy(left, right, Closure key) {
    return pairUp(left, right, key, key, false)
}

def combineBy(left, right, Closure key, Map options) {
    return pairUp(left, right, key, key, false)
}

def combineBy(left, right, Closure leftKey, Closure rightKey) {
    return pairUp(left, right, leftKey, rightKey, false)
}

def combineBy(left, right, Closure leftKey, Closure rightKey, Map options) {
    return pairUp(left, right, leftKey, rightKey, false)
}

// Every left item with every right item of an equal key, once both channels are complete.
def pairUp(left, right, Closure leftKey, Closure rightKey, remainder) {
    def lefts = left.map { item -> [bidsKey(leftKey.call(item)), item] }.toList().map { pairs -> [pairs] }
    def rights = right.map { item -> [bidsKey(rightKey.call(item)), item] }.toList().map { pairs -> [pairs] }
    return lefts.combine(rights).flatMap { both -> bidsPairs(both[0], both[1], remainder) }
}
