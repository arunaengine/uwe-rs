/*
 * The sqlInsert operator of nf-sqldb 0.7.1, which takes the channel first.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { sqlWriter } from 'plugin/nf-sqldb'

// Passes each item on once it is in the current batch; the end of the channel commits the rest.
def sqlInsert(source, Map options = [:]) {
    def writer = sqlWriter(options)
    def passed = source.map { item -> writer.add(item); item }
    passed.subscribe(onComplete: { writer.close() })
    return passed
}
