// Prints the names of the methods that Groovy 4 finds for values of some Java classes, with
// the methods that Groovy adds (DefaultGroovyMethods and the like): one line per class, the
// class, a colon and the names in order.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

def values = [
    new ArrayList(),
    new LinkedHashMap(),
    'text',
    1,
    1L,
    1.5d,
    1.5G,
    true,
]
for (value in values) {
    def meta = GroovySystem.metaClassRegistry.getMetaClass(value.getClass())
    def names = (meta.methods*.name + meta.metaMethods*.name).unique().sort()
    println "${value.getClass().name}: ${names.join(' ')}"
}
