/*
 * The functions and channel operators of the nf-emoji plugin 0.2.0, in Nextflow script.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

def printEmoji(text) {
    println text
}

// Prints each item after the emoji and passes it on.
def emojiView(source, Map options) {
    def emoji = options.emoji ?: '🔍'
    return source.map { item ->
        println "${emoji} ${item}"
        item
    }
}

// Prints each item after the emoji and tag, always, and passes it on.
def emojiDump(source, Map options) {
    def emoji = options.emoji ?: '🔖'
    def tag = options.tag ?: 'debug'
    return source.map { item ->
        println "[${emoji} ${tag}] ${item}"
        item
    }
}
