<!-- Implementer specification of the Groovy runtime semantics that Nextflow strict-syntax scripts depend on. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Groovy runtime semantics for Nextflow strict syntax

This document tells an implementer how values, operators, closures and the standard library behave when Nextflow runs a strict-syntax script. The goal is a native Rust evaluator that produces the same values as Nextflow, and so the same task Bash scripts. Small differences matter. For example, `7 / 2` is the decimal `3.5`, not the float `3.5`. And `"${[a: 1]}"` renders as `[a:1]`, not `{a=1}`.

Channel operator semantics, file staging, executors and the config file language are out of scope. They are only described where they decide how a value looks inside a task script.

## 1. Sources and how claims are marked

- **Nextflow source.** The checkout at `/tmp/nf-research/nextflow`, commit `bbf6ff0` (2026-09-25). The file `VERSION` says `26.09.1-edge`. All Nextflow paths in this document are relative to the checkout root.
- **Groovy source.** The checkout pins Groovy 4.0.33 (`modules/nextflow/build.gradle:32`, `modules/nf-commons/build.gradle:30`). The Groovy source is not part of the checkout. It was read from `org.apache.groovy:groovy:4.0.33:sources` (Maven Central). Groovy paths are written as "Groovy `org/codehaus/...:line`". They are relative to the root of that source jar.
- **Observed.** Test scripts were run with the installed Nextflow 26.04.6 (`~/.nextflow/framework/26.04.6`). That runtime bundles Groovy 4.0.31 (`META-INF/groovy-release-info.properties` in the framework jar) and ran on OpenJDK 21. Claims marked "observed" come from these runs. The checkout is newer than 26.04.6. Where the checkout source differs from what 26.04.6 did, the difference is called out, and the checkout wins.
- **Java platform.** Some behaviour comes from the JDK (for example `Double.toString`). The JDK source is not available locally. Such claims are marked observed when a run confirmed them, and **unverified** otherwise.
- **Review.** This revision was checked claim by claim against the checkout and the Groovy 4.0.33 source. Many claims were run again with 26.04.6. Wrong claims were fixed in place. Observed claims that were not run again keep their original observation.
- Anything else that could not be confirmed is marked **unverified**.

## 2. How a strict-syntax script runs

The `nf-lang` module parses and checks strict syntax. It then converts the script to an ordinary Groovy AST. Groovy 4 compiles that AST and runs it with dynamic dispatch (`modules/nextflow/src/main/groovy/nextflow/script/parser/v2/ScriptCompiler.java:232-397`). So the runtime semantics are Groovy 4 dynamic semantics, plus Nextflow extension methods, plus a few AST rewrites. The evaluator must reproduce all three.

`ScriptCompiler.convertToGroovy` (`ScriptCompiler.java:371-397`) runs these AST visitors in this order: `ScriptToGroovyVisitor`, `StripTypesVisitor`, `PathCompareVisitor`, `OpCriteriaVisitor`, `GStringToStringVisitor` (`ScriptCompiler.java:392-396`). The rewrites that change behaviour are:

1. **GString to String.** A GString expression becomes `"...".toString()` (`modules/nf-lang/src/main/java/nextflow/script/control/GStringToStringVisitor.java`). The visitor does not descend into the values of a GString that it has replaced. So a GString written inside the `${...}` of another GString stays a `GStringImpl` at run time. This includes a GString inside a closure within `${...}`. Observed: `"${ "${1}".getClass().getSimpleName() }"` is `GStringImpl`, and `"${ [1, 2].collect { v -> "v${v}" }*.getClass()*.getSimpleName() }"` is `[GStringImpl, GStringImpl]`. All other GStrings are rendered eagerly, where they are evaluated. Observed: `"${1}" instanceof String` is `true`, and a map built with `["${k}": 1]` has a `String` key. A surviving GString renders like the equal `String`, and `==` treats the two as equal. But `GString.equals` is false for a `String`, and its hash code differs (Groovy `groovy/lang/GString.java:147-161`). So it fails as a map key and in `contains`. Observed: `"${ ['a'].contains("${'a'}") }"` is `false`, and `"${ ["${k}": 1]['k'] }"` is `null`.
2. **Comparison operators.** `==`, `!=`, `<`, `<=`, `>` and `>=` become calls to `nextflow.util.LangHelpers.compareEqual` and friends (`modules/nf-lang/src/main/java/nextflow/script/control/PathCompareVisitor.java`, `modules/nextflow/src/main/groovy/nextflow/util/LangHelpers.java:47-262`). `LangHelpers` is a Java source file, so its `left==right` is an identity test. `<=>` is not rewritten.
3. **Task script escaping.** In a process body of type `script` and in `stub:`, every value interpolated into a GString first goes through `LangHelpers.applyPathEscapeAware` (`TaskCmdXformVisitor.java:52-57`, `ProcessToGroovyVisitorV1.java:56-58`, `ProcessToGroovyVisitorV2.java:75-77`, `LangHelpers.java:278-283`). This covers every GString in the section, including nested ones and GStrings assigned to local variables (observed: `def inner = "in=${reads}"` in a script section gave `in=a\ b.fq c.fq`). `exec:` and `shell:` bodies are not transformed. See section 6.7.
4. **Type stripping** (`StripTypesVisitor.java`).
   - Casts to `Channel` (both `nextflow.Channel` and the typed `Channel`), `Record`, `Tuple` and `Value` are removed (`:62-68`, `:116-120`).
   - A local declaration with one of these types, or with a user record type, loses its type (`:162-168`).
   - A cast to a type with generic arguments, such as `x as List<Sample>`, becomes `TypeHelper.asType(x, <hidden class>, '<field>')`. A cast to a user record type becomes `TypeHelper.asType(x, RecordType)` (`:116-129`). See section 10.10.
   - `instanceof` against a user record type becomes `TypeHelper.instanceofRecordType` (`:179-184`).
   - Function parameters with a user record type are erased to `def` (`:84-92`).
   - All other type annotations stay. A typed local variable is converted by Groovy on assignment. Observed: `def x: Integer = 5L` gives the `Integer` 5, `def x: String = 5` gives `'5'`, and `def x: List = [1] as Set` throws `GroovyCastException`.
   - A typed function parameter keeps its Java type, so Groovy method dispatch checks it at run time. Observed: `def typed(x: Integer)` called with `'a'`, or with `5L`, throws `MissingMethodException`.
5. **Imports.** The compiler imports `java.nio.file.Path`, `nextflow.script.types.Value`, `nextflow.util.Duration`, `nextflow.util.MemoryUnit` and `nextflow.util.VersionNumber`. It also imports every static member of `nextflow.Nextflow` (`ScriptCompiler.java:234-240`, `:381-384`). The name `channel` is bound to `nextflow.dataflow.ChannelNamespace` when static typing is enabled (`nextflow.enable.types`), and to `nextflow.Channel` otherwise (`ScriptCompiler.java:386-389`). The Groovy default imports also apply: `java.lang.*`, `java.util.*`, `java.io.*`, `java.net.*`, `groovy.lang.*`, `groovy.util.*`, `java.math.BigDecimal` and `java.math.BigInteger` (`docs/reference/stdlib-groovy.mdx`).
6. **Dynamic directives.** In a legacy process, a directive may be wrapped in a closure, so it is evaluated once per task. This happens when the directive has exactly one argument, the argument is not already a closure, and it refers to a variable (`ProcessToGroovyVisitorV1.java:89-105`, `ScriptToGroovyHelper.java:137-144`). The legacy process never wraps `executor`, `label`, `maxForks`, `module`, `pod`, `publishDir` or `secret` (`ProcessToGroovyVisitorV1.java:79-87`). A typed process (`nextflow.enable.types = true`) has no such exclusion list (`ProcessToGroovyVisitorV2.java:101-111`). See section 9.7.
7. **Closure parameters.** The parser gives both `{ -> x }` and `{ x }` an empty parameter array (`ScriptAstBuilder.java:1993-1995`, `:1813-1817`). Groovy turns an empty array into one optional parameter named `it` (Groovy `org/codehaus/groovy/classgen/asm/ClosureWriter.java:194-203`). So in Nextflow, `{ -> 5 }` accepts zero or one argument. See section 9.1.
8. **Branch and multiMap criteria.** `OpCriteriaVisitor` looks at every call named `branch` or `multiMap` with one closure argument, and at `branchCriteria` and `multiMapCriteria`. It rewrites the closure into a criteria object (`modules/nf-lang/src/main/java/nextflow/script/control/OpCriteriaVisitor.java`). This belongs to channel operators and is out of scope. But an evaluator must not treat these closures as plain closures.

Two runtime hooks add behaviour to existing classes (`modules/nextflow/src/main/java/groovy/runtime/metaclass/CustomMetaClassCreationHandle.java:55-83`):

- Every `Number` subclass gets `NumberDelegatingMetaClass`. It adds the unit properties such as `2.GB` and `1.h` (`NumberDelegatingMetaClass.java:37-86`).
- `File`, `Path` and channel classes get `NextflowDelegatingMetaClass`. It adds `isEmpty()` for paths and plugin extension methods (`NextflowDelegatingMetaClass.java`).

Groovy extension modules add methods to JDK classes. Nextflow registers `nextflow.extension.Bolts` and `nextflow.extension.FilesEx` (`modules/nf-commons/src/resources/META-INF/services/org.codehaus.groovy.runtime.ExtensionModule`). It also registers `ChannelEx`, `SplitterEx` and `SlurperEx` (`modules/nextflow/src/main/resources/META-INF/services/org.codehaus.groovy.runtime.ExtensionModule`).

## 3. Runtime value types

The type checker groups runtime classes into a few Nextflow types (`modules/nf-lang/src/main/java/nextflow/script/dsl/Types.java:406-441`). At run time the classes stay distinct, and the difference is visible (for example, `1L instanceof Integer` is `false`, observed).

| Nextflow type | Runtime classes an evaluator must model | Notes |
| --- | --- | --- |
| Integer | `Integer` (32-bit), `Long` (64-bit), `BigInteger` | The checker treats all as `Integer` (`Types.java:419-423`). |
| Float | `BigDecimal`, `Double`, `Float` | The checker treats all as `Float` (`Types.java:413-417`). Decimal literals are `BigDecimal`. |
| Boolean | `Boolean` | |
| String | `String`, rarely `GStringImpl` | GStrings are converted to `String`, except nested ones (section 2). |
| null | `null` | |
| List | `ArrayList` (literals), `IntRange`, `NumberRange`, `ObjectRange`, `EmptyRange` (ranges), `ArrayTuple` (tuples), `String[]` (from `split`), `LinkedList` (from `file()` globs, `Nextflow.groovy:71-80`), `BlankSeparatedList` (multi-file task inputs) | All implement `java.util.List` except `String[]`. That is a Java array, but it behaves like a list for indexing and iteration (observed). |
| Set | `LinkedHashSet` (`as Set`, set `+`/`-`), `HashSet` (`toSet()`) | Iteration order differs, see section 10.4. |
| Bag | any `Collection`, for example `LinkedHashMap.values()` | `Types.java` maps a plain `Collection` to `Iterable`. |
| Map | `LinkedHashMap` (literals), `RecordMap` (records), `TreeMap` (`Map.sort()`), JSON maps | Literals keep insertion order. |
| Record | `nextflow.util.RecordMap` | A `LinkedHashMap` that throws on `put`, `putAll`, `remove` and `clear` (`modules/nf-commons/src/main/nextflow/util/RecordMap.java`). Other `LinkedHashMap` mutators are not overridden in that file. `get(k, default)` calls `put` and throws (observed). |
| Tuple | `nextflow.util.ArrayTuple` | An `ArrayList` that throws on `add`, `addAll`, `remove`, `removeAll`, `retainAll`, `clear`, `set` and iterator changes (`modules/nf-commons/src/main/nextflow/util/ArrayTuple.java`). `sort`, `removeIf` and `replaceAll` are not blocked. Observed: `tuple(3, 1, 2).sort()` sorts the tuple in place, and `removeIf` removes elements. |
| Closure | `groovy.lang.Closure` | |
| Path | `java.nio.file.Path` (`UnixPath` locally), `TaskPath` inside a task | |
| MemoryUnit | `nextflow.util.MemoryUnit` | A non-negative `long` byte count (`modules/nf-commons/src/main/nextflow/util/MemoryUnit.groovy`). |
| Duration | `nextflow.util.Duration` | A non-negative `long` millisecond count (`modules/nf-commons/src/main/nextflow/util/Duration.groovy`). |
| VersionNumber | `nextflow.util.VersionNumber` | `nextflow.version` (`modules/nf-commons/src/main/nextflow/util/VersionNumber.groovy`). |
| (regex) | `java.util.regex.Matcher`, `java.util.regex.Pattern` | From `=~` and `~'...'`. |

## 4. Literals

### 4.1 Numbers

The lexer rules are in `modules/nf-lang/src/main/antlr/ScriptLexer.g4:382-590`. The AST builder turns the literal text into a value with Groovy's `Numbers.parseInteger` and `Numbers.parseDecimal` (`ScriptAstBuilder.java:1626-1654`; Groovy `org/codehaus/groovy/syntax/Numbers.java:100-194`, `:206-243`). A parse failure becomes a compile error.

Integer literals:

- Underscores are removed first.
- Radix: `0x`/`0X` is hex, `0b`/`0B` is binary, and a leading `0` followed by more digits is octal. A leading `0` followed by `8` or `9` is a lexer error ("Invalid octal number", `ScriptLexer.g4:391`).
- Suffix `i`/`I` gives `Integer`, `l`/`L` gives `Long`, and `g`/`G` gives `BigInteger`. The range check for `i` and `l` only applies to decimal literals. For hex, octal and binary, the value is truncated with `intValue()` or `longValue()` (Groovy `Numbers.java:170-181`).
- No suffix gives the narrowest of `Integer`, `Long` and `BigInteger` that holds the value (Groovy `Numbers.java:184-192`).

Floating-point literals (a literal with `.digits`, an exponent, or a float suffix):

- Suffix `d`/`D` gives `Double`, and `f`/`F` gives `Float`. A value whose magnitude is too large is a compile error (Groovy `Numbers.java:224-236`).
- No suffix, or `g`/`G`, gives a `BigDecimal` built from the text, so the scale of the text is kept: `1.50` has scale 2 and renders as `1.50`.
- `1d` and `1f` are floating-point literals (`Digits FloatTypeSuffix`, `ScriptLexer.g4:537-541`). `1g` is an integer literal (`BigInteger`), because the integer rule comes first in the lexer (`ScriptLexer.g4:382`, `:529`).
- A leading sign is not part of the literal. `-x` is a unary minus applied at run time (`ScriptAstBuilder.java:1427-1435`). So `-2147483648` is a `Long`, because `2147483648` is a `Long` (observed).

Observed literal types (26.04.6):

| Literal | Value | Class |
| --- | --- | --- |
| `1`, `2147483647` | 1, 2147483647 | Integer |
| `2147483648`, `9223372036854775807` | | Long |
| `9223372036854775808` | | BigInteger |
| `-2147483648` | -2147483648 | Long |
| `10L` / `10i` / `10G` | 10 | Long / Integer / BigInteger |
| `0x1F`, `0b101`, `017` | 31, 5, 15 | Integer |
| `1_000_000` | 1000000 | Integer |
| `1.5`, `1.50`, `1.5G`, `0.0` | `1.5`, `1.50`, `1.5`, `0.0` | BigDecimal |
| `1e3`, `1.0e3`, `1.59e7` | `1E+3`, `1.0E+3`, `1.59E+7` | BigDecimal |
| `1e-3` | `0.001` | BigDecimal |
| `1.5d`, `1d`, `1e3d` | `1.5`, `1.0`, `1000.0` | Double |
| `1.5f`, `1f` | `1.5`, `1.0` | Float |

### 4.2 Booleans and null

`true` and `false` are `Boolean` constants. `null` is the null constant (`ScriptAstBuilder.java:1617-1621`).

### 4.3 Strings

Kinds of string literal (`ScriptLexer.g4:142-240`, `docs/reference/syntax.mdx`, section "String"):

- `'...'` and `'''...'''`: plain strings, never interpolated.
- `"..."` and `"""..."""`: interpolated when they contain an unescaped `$`; otherwise plain. An escaped `\$` does not start interpolation (`ScriptLexer.g4:199-206`). Both are `String` at run time (section 2, with the nesting exception).
- `/.../` slashy strings: plain, never interpolated in strict syntax.
  - A `$` is allowed only if it is not followed by an identifier character (`ScriptLexer.g4:233-237`).
  - A `/` starts a slashy string only where the lexer allows a regex (`isRegexAllowed()`), and not when the next character is `*` (`ScriptLexer.g4:144`).
  - A slashy string cannot be empty, because `//` starts a comment.
  - Escapes: `\/` becomes `/`, `\uHHHH` becomes the character, and a backslash before a newline is removed together with the newline. Every other backslash stays as it is (Groovy `org/apache/groovy/parser/antlr4/util/StringUtils.java:36-49`). Observed: `/Ab/` is `Ab`, and a backslash-newline inside a slashy string was removed. The escape check below is skipped for slashy strings (`EscapeUtils.java:51-53`).
  - Newlines: the lexer accepts newlines inside a slashy string (`ScriptLexer.g4:233-237`), and 26.04.6 accepted a multi-line slashy string (observed). `docs/strict-syntax.mdx` says a slashy string cannot span lines. Do not rely on either.
- Dollar-slashy strings `$/.../$` are not supported (`docs/strict-syntax.mdx`).
- Triple-quoted strings keep all characters, including the first newline and indentation. In triple-quoted and slashy strings, each CR LF pair becomes LF. A lone CR is kept (`StringUtils.removeCR` replaces `\r\n` only; `ScriptAstBuilder.java:1668-1687`). Single- and double-quoted strings cannot contain a raw CR or LF (`ScriptLexer.g4:199-213`).

Escapes in single-, double- and triple-quoted strings (`docs/reference/syntax.mdx`; `modules/nf-lang/src/main/java/nextflow/script/parser/EscapeUtils.java:33`):

- `\b \t \n \f \r \s \" \' \\ \$`. `\s` is a space, and `\$` is also valid in single quotes (observed: `'\$x'` is `$x`).
- A backslash before a newline: the newline is removed, including right after an opening `"""`.
- `\uHHHH`, with exactly one `u` (`ScriptLexer.g4:623-627`).
- Octal `\N`, `\NN` and `\NNN` (first digit 0-3 for three digits).

Any other escape is a syntax error in the checkout (`EscapeUtils.checkEscapes`, called from `ScriptAstBuilder.java:1658` and `:1761`). Observed: 26.04.6 accepted `"a\qb"` and printed `ab`. Follow the checkout and reject it.

Interpolation (`ScriptLexer.g4:157-200`, `ScriptAstBuilder.java:1689-1797`):

- `${expr}` inserts any expression.
- `$name` or `$name.a.b` inserts a variable followed by a greedy chain of `.identifier` property accesses. The chain stops at the first character that is not `.` followed by an identifier start. So `"$m.id-x"` is `m.id` then `-x`, and `"$m.id."` keeps the final dot as text. But `"$prefix.bam"` means `prefix.bam`, a property access. For a `String` it fails with `MissingPropertyException` (observed). `"$m.id.size()"` means `m.id.size` then the text `()` (observed error). A `$` cannot be part of an identifier inside a GString, so `"$a$b"` is `$a` then `$b` (`ScriptLexer.g4:814-834`).
- A `$` that is not followed by `{` or an identifier start is a lexer error in the checkout (`GStringUnexpectedChar`, `ScriptLexer.g4:174-176`). Observed: 26.04.6 accepted `"a $ b"` and printed `a b`. Follow the checkout.
- Each inserted value is converted with the rules of section 6.
- Interpolation is eager: the string is built when the expression is evaluated (`docs/reference/semantics.mdx`, section "Strings"; also section 2 above).

### 4.4 Lists, maps, ranges and closures

- `[a, b]` creates an `ArrayList`. A trailing comma is allowed (`ScriptParser.g4:728-730`).
- `[k: v]` creates a `LinkedHashMap` in source order. `[:]` is an empty map (`ScriptParser.g4:737-755`, `ScriptAstBuilder.java:1863-1894`). A key is a keyword or any primary expression (`ScriptParser.g4:751-754`). Key rules:
  - A bare identifier is the string of its name: `[x: 1]` has the key `'x'`.
  - Keywords `true`, `false`, `null`, `in`, `def`, `as`, `process`, primitive type names and the other names in the `keywords` rule are strings too (`ScriptParser.g4:826-861`). `[true: 1]` has the key `'true'` (observed).
  - Number literals stay numbers: `[7: 'a']` has an `Integer` key, and `[1.5: 'a']` has a `BigDecimal` key (observed).
  - String and GString literals are `String` keys.
  - `(expr)` uses the value of the expression: `[(k): 1]`.
- `a..b` and `a..<b` create a range (`ScriptAstBuilder.java:1398-1407`) through Groovy `ScriptBytecodeAdapter.createRange` (Groovy `org/codehaus/groovy/runtime/ScriptBytecodeAdapter.java:645-697`):
  - An exclusive range with equal ends is an `EmptyRange`: `(1..<1)` is empty.
  - Two `Integer` ends give a `groovy.lang.IntRange`.
  - Two other numbers give a `groovy.lang.NumberRange` that steps by adding 1. Observed: `(1.5..3)` is `[1.5, 2.5]`, and `(1L..3)` is a `NumberRange`.
  - Other values give an `ObjectRange` that steps with `next()` (observed: `('a'..'c')` is `[a, b, c]`).
  - If `a > b`, the range counts down: `(3..1)` is `[3, 2, 1]`. `(0..<-2)` is `[0, -1]` (observed).
  - Ranges are lists.
- `{ params -> body }` creates a closure (section 9).

### 4.5 Unit suffixes on numbers

`n.GB`, `n.h` and similar are property reads on a number. When the number has no such property, `NumberDelegatingMetaClass.getProperty` (`NumberDelegatingMetaClass.java:37-54`) does this:

1. If `property.toLowerCase()` is a duration unit, return `new Duration(n.toString() + lowerCaseUnit)`. Duration units are `ms milli millis s sec second seconds m min minute minutes h hour hours d day days` (`Duration.groovy:43-66`).
2. Else if `property.toUpperCase()` is a memory unit, return `new MemoryUnit(n.toString() + upperCaseUnit)`. Memory units are `B KB MB GB TB PB EB ZB` (`MemoryUnit.groovy:38`).
3. Else throw the original `MissingPropertyException`.

Consequences:

- `1.m` is one minute, not a memory unit. `1.b` and `1.kb` are memory units. `1.Hours` is one hour. `1.K` is an error. `1.5.GB` is `Math.round(1.5 * 1024^3)` bytes (all observed).
- The number is rendered with `toString()`. So a `BigDecimal` in exponent form such as `1e3` gives `"1E+3GB"`, which the memory parser rejects (observed: `1e3.GB` throws `IllegalArgumentException` "Not a valid FileSize value").
- `1.ZB` is `2^70` bytes as a `double`. `Math.round` clamps that to `Long.MAX_VALUE`, so `1.ZB` renders as `8 EB` (observed; `MemoryUnit.groovy:95`).

The string parsers are:

- **`MemoryUnit(String)`** (`MemoryUnit.groovy:71-99`).
  - The whole string must match `([0-9\.]+)\s*(\S)?B?`, and the unit letter is upper-cased.
  - No unit, or `B`, means `Long.parseLong` of the number.
  - Otherwise the unit is looked up in `B KB MB GB TB PB EB ZB`, first as the letter, then as the letter plus `B`. The size is `Math.round(Double.parseDouble(number) * 1024^index)`.
  - The optional trailing `B` is case-sensitive.
  - Observed: `'2g'`, `'2 GB'` and `'1.5 GB'` work. `'2gb'`, `'2 kb'` and `'2 GiB'` throw `IllegalArgumentException`. `'1.5B'` throws `NumberFormatException`. `'2'` is 2 bytes.
  - A negative size cannot be written, because the pattern has no sign.
- **`Duration(String)`** (`Duration.groovy:102-181`).
  - The parser repeatedly matches `^(\d+\.?\d*)\s*([a-zA-Z]+)` at the start of the string. For each part it adds `Math.round(number * unitMillis)`, then trims the rest.
  - Unit names are compared exactly, in lower case. An unknown unit raises `IllegalStateException`, which the constructor wraps into `IllegalArgumentException`.
  - If the first match fails, the legacy form `hh:mm:ss` is tried on the original string.
  - Observed: `'1day 6hours 3minutes 30seconds'` is `1d 6h 3m 30s`, `'2h30m'` is `2h 30m`, `'1.5h'` is `1h 30m`, `'1 hour'` is `1h`, and `'01:30:00'` is `1h 30m`. `'1H'` throws `IllegalArgumentException`.

## 5. Arithmetic

### 5.1 Promotion

Binary arithmetic on two numbers picks one "math" from the classes of both operands (Groovy `org/codehaus/groovy/runtime/typehandling/NumberMath.java:222-262`):

1. If either operand is `Double` or `Float`: floating-point math on `double` values. The result is a `Double`.
2. Else if either is `BigDecimal`: `BigDecimal` math.
3. Else if either is `BigInteger`: `BigInteger` math.
4. Else if either is `Long`: `long` math. The result is a `Long`.
5. Else: `int` math. The result is an `Integer`.

There is no automatic widening on overflow. `Integer` and `Long` results wrap around in two's complement (Groovy `IntegerMath.java:36-48`, `LongMath.java:36-48`). Observed: `2147483647 + 1` is `-2147483648`, `2147483647 * 2` is `-2`, and `9223372036854775807 + 1` is `-9223372036854775808`. But `2147483647 + 1L` is the `Long` `2147483648` (observed).

A `Float` never survives arithmetic: `1.5f + 1` is the `Double` `2.5`, and `1.1f + 0` is `1.100000023841858` (observed).

### 5.2 Addition, subtraction, multiplication

- `int` and `long` math: Java `+ - *` with wrap-around.
- `BigInteger` math: exact.
- `BigDecimal` math: exact Java `BigDecimal` operations (Groovy `BigDecimalMath.java:50-62`). The result scale is the larger scale for `+` and `-`, and the sum of the scales for `*` (Java behaviour). Observed: `1.10 + 1.20` is `2.30`, `1.5 * 2` is `3.0`, `2.50 * 2` is `5.00`, `1.0 * 1.0` is `1.00`, `1.5 - 1.5` is `0.0`, and `0.1 + 0.2` is `0.3`.
- Floating-point math: Java `double` arithmetic. Observed: `0.1d + 0.2d` is `0.30000000000000004`, and `3 * 1.1d` is `3.3000000000000003`.

### 5.3 Division

`/` never does integer division. If both operands are `Integer`, `Long`, `BigInteger` or `BigDecimal`, `BigDecimalMath.divideImpl` does the division (Groovy `BigDecimalMath.java:65-78`; `IntegerMath`, `LongMath` and `BigIntegerMath` all delegate to it):

1. Convert both operands to `BigDecimal`. Integers get scale 0.
2. Try `left.divide(right)`, the exact division. If the exact quotient has a finite decimal expansion, return it. Java picks the smallest scale that is at least `left.scale - right.scale` and represents the quotient exactly. This scale can be negative. A negative scale renders in exponent form (section 6.2). Observed: `10 / 0.5` is `2E+1`, and `100 / 0.5` is `2.0E+2`.
3. If the quotient does not terminate, divide again with a `MathContext` of precision `max(left.precision, right.precision) + 10`. That context rounds `HALF_UP`, the Java default for `MathContext(int)`. Then, if the result scale is larger than `max(left.scale, right.scale, 10)`, set the scale to that value with `HALF_UP`. Note the two rounding steps.
4. Division by zero throws `ArithmeticException`. The message is "Division by zero", or "Division undefined" for `0 / 0` (observed).

If either operand is `Double` or `Float`, it is `double` division. Then `x / 0` gives `Infinity` or `NaN` (observed).

Observed results:

| Expression | Result | Class |
| --- | --- | --- |
| `7 / 2` | `3.5` | BigDecimal |
| `6 / 2` | `3` | BigDecimal |
| `6 / 2.0` | `3` | BigDecimal |
| `[1, 2, 3].sum() / 3` | `2` | BigDecimal |
| `10 / 4` | `2.5` | BigDecimal |
| `1 / 8` | `0.125` | BigDecimal |
| `1 / 1024` | `0.0009765625` | BigDecimal |
| `1 / 3` | `0.3333333333` | BigDecimal |
| `2 / 3` | `0.6666666667` | BigDecimal |
| `10 / 3` | `3.3333333333` | BigDecimal |
| `100 / 3` | `33.3333333333` | BigDecimal |
| `123456789012 / 7` | `17636684144.5714285714` | BigDecimal |
| `1.00000000000 / 3` | `0.33333333333` | BigDecimal (scale 11 kept) |
| `10 / 0.5`, `100 / 0.5` | `2E+1`, `2.0E+2` | BigDecimal (negative scale) |
| `7d / 2`, `7 / 2d`, `7f / 2` | `3.5` | Double |
| `1 / 0` | ArithmeticException | |
| `1d / 0`, `0d / 0` | `Infinity`, `NaN` | Double |

The documentation says integer division "produces a floating-point result" (`docs/reference/semantics.mdx`, section "Numbers"). At run time that result is a `BigDecimal`, not a `Double`.

### 5.4 `intdiv`, `%` and `**`

- **`a.intdiv(b)`** works only when both operands are `Integer`, `Long` or `BigInteger`. It truncates toward zero, like Java `/` (observed: `(-7).intdiv(2)` is `-3`). If either operand is a `BigDecimal` or `Double`, it throws `UnsupportedOperationException` (Groovy `NumberMath.java:303-305`; `BigDecimalMath` and `FloatingPointMath` do not override it; observed: `7.5.intdiv(2)` and `7d.intdiv(2)` fail). Division by zero throws `ArithmeticException`.
- **`a % b`** calls `mod` (Groovy `IntegerMath.java:83`, `LongMath.java:68`, `BigIntegerMath.java:65`, `BigDecimalMath.java:96`, `FloatingPointMath.java:61`):
  - `Integer`/`Long`: Java remainder, with the sign of the dividend. `-7 % 3` is `-1`, and `7 % -3` is `1` (observed).
  - `BigInteger`: `BigInteger.mod`, never negative. `(-7G) % 3` is `2` (observed).
  - `BigDecimal`: `BigDecimal.remainder`. `7.5 % 2` is `1.5`.
  - `Double`: Java `%`. `7.5d % 2` is `1.5`.
- **`a ** b`** (Groovy `DefaultGroovyMethods.java:16193-16305`):
  - `Integer ** Integer` with a non-negative exponent is exact, via `BigInteger`. The result is an `Integer` if it fits, else a `BigInteger` (not a `Long`). `2 ** 31` is the `BigInteger` 2147483648 (observed).
  - `Long ** Integer`: a `Long` if it fits, else a `BigInteger`.
  - `BigInteger ** Integer` and `BigDecimal ** Integer` with a non-negative exponent use exact `pow`. `1.5 ** 2` is `2.25`.
  - `BigInteger ** BigInteger` with an exponent from 0 to `Integer.MAX_VALUE` is exact (Groovy `DefaultGroovyMethods.java:16296-16302`; observed: `2G ** 2G` is the `BigInteger` 4).
  - A negative `Integer` exponent, and every other combination (for example `Integer ** Long`), uses `Math.pow` on doubles. The result is an `Integer` if it is an exact `int` value, else a `Long` if it is an exact `long` value, else a `Double`. So `2 ** -1` is `0.5` (Double), `4 ** 0.5` is `2` (Integer), `2d ** 2` is `4` (Integer, observed), and `2 ** 3L` is `8` (Integer, observed).
  - `**` is left-associative: `2 ** 3 ** 2` is `64` (observed).

### 5.5 Unary, bitwise and shift operators

- Unary `-` keeps the class: `Integer` stays `Integer` (and `-Integer.MIN_VALUE` wraps), and `BigDecimal` stays `BigDecimal` with its scale (Groovy `org/codehaus/groovy/runtime/InvokerHelper.java:238-278`; observed: `-(1.50)` is `-1.50`). Unary `-` on an `ArrayList` negates each element (observed: `-[1, 2]` is `[-1, -2]`). On any other object it calls `negative()`.
- `~`, `&`, `|` and `^` work on `Integer`, `Long` and `BigInteger`. On `BigDecimal` or `Double` they throw `UnsupportedOperationException`, because only `IntegerMath`, `LongMath` and `BigIntegerMath` implement them (Groovy `NumberMath.java:283-301`). On `Boolean`, `&`, `|` and `^` are logical and do not short-circuit (`true ^ true` is `false`, observed). `~` on a string creates a `Pattern`.
- `<<`, `>>` and `>>>` pick the math from the left operand only (Groovy `NumberMath.java:96-128`). On an `Integer` they use Java `int` shifts, so the distance is taken modulo 32: `1 << 32` is `1` (observed). A `Double` or `BigDecimal` distance throws `UnsupportedOperationException`. `>>>` on a `BigInteger` also throws, because `BigIntegerMath` does not implement it (Groovy `BigIntegerMath.java:100-108`).

### 5.6 MemoryUnit and Duration arithmetic

Sources: `MemoryUnit.groovy:121-135`, `Duration.groovy:332-346` and `NumberDelegatingMetaClass.java:56-86`.

- `unit + unit`, `unit - unit`: a new value from the sum or difference. A negative result fails an `assert`, which raises a Java `AssertionError`. That error is not caught by `catch( e )` and ends the run (observed: `2.GB - 3.GB`, and `1.s - 2.s` gives "Duration unit cannot be a negative number"). `MemoryUnit` treats a `null` operand as zero; `Duration` does not check for `null`.
- `unit * n`: `new MemoryUnit((long)(size * n))`. With a `BigDecimal` or `Double` factor, the product is truncated toward zero.
- `n * unit`: the number metaclass supports it as a fallback, for `multiply` only (observed: `2 * 1.h` is `2h`, and `1.5d * 1.GB` is `1.5 GB`).
- `unit / n`: `Math.round((double)(size / n))`, where `size / n` is the Groovy division of section 5.3.
- `unit + n` and `unit / unit` fail with `MissingMethodException` (observed: `1.GB + 1`, `1.h / 1.h`).
- Observed: `2.GB * 1.5` is `3 GB`, `1.GB * 0.5` is `512 MB`, `2.GB / 3` is `682.7 MB`, `1.h * 1.5` is `1h 30m`, and `1.h / 7` is `8m 34s`.
- Comparisons of units with a zero value can throw. See section 8.2.

### 5.7 Converting between number types

| Operation | Behaviour | Source |
| --- | --- | --- |
| `x as Integer` from `BigDecimal` / `Double` | truncates toward zero (`3.7 as Integer` is 3, `-3.7 as Integer` is -3) | observed |
| `x.intValue()`, `x.toInteger()` | Java `intValue()`, truncates | observed (`1.9.toInteger()` is 1) |
| `bd.round()` on a BigDecimal | `BigDecimal`, `HALF_UP`, scale 0 (`2.5.round()` is `3`, `(-2.5).round()` is `-3`) | Groovy `DefaultGroovyMethods.java:17287-17309`; observed |
| `d.round()` on a Double | `Long` from `Math.round`, which rounds half up toward positive infinity (`3.7d.round()` is 4, `2.5d.round()` is 3, `(-2.5d).round()` is -2) | Groovy `DefaultGroovyMethods.java:17229-17231`; observed |
| `Math.round(x)` | `Long`, half up toward positive infinity (`Math.round(-2.5)` is -2) | observed |
| `Math.ceil`, `Math.floor`, `Math.sqrt`, `Math.pow`, `Math.log10` | `Double` | observed |
| `Math.max(1, 2L)` | Java overload resolution, `Long` 2 | observed |
| `Math.min(1.5, 2)` | fails: ambiguous overload for `BigDecimal` | observed |

## 6. String conversion

Task scripts are strings, so this section decides the Bash text. There are two related Groovy functions. They differ in one case: ranges.

### 6.1 The two formatting functions

`FormatHelper.write(out, value)` is used for every value inserted into a GString (Groovy `org/codehaus/groovy/runtime/GStringUtil.java:77-102`, `FormatHelper.java:483-513`). It dispatches in this order:

1. `String`: the string itself.
2. `Object[]` (for example the `String[]` from `split`): array format.
3. `Map`: map format (6.3).
4. `Collection`: list format (6.3). A range is a collection, so `"${1..3}"` is `[1, 2, 3]`. A `BlankSeparatedList` is a collection too (section 6.4).
5. Other writable objects and streams (not relevant here).
6. Anything else: `FormatHelper.toString(value)`.

A closure inserted into a GString is special (Groovy `GStringUtil.java:84-95`):

- A closure that takes no parameters is called, and its result is written.
- A closure with one parameter is called with the output writer, and what it writes is inserted. Its return value is dropped.
- A closure with two or more parameters is an error.

Because of section 2 point 7, every Nextflow closure without explicit parameters has one optional parameter. So `"${{ -> 'x' }}"` inserts nothing (observed: empty string).

`FormatHelper.toString(value)` is the same as `format(value, false, -1, false)` (Groovy `FormatHelper.java:78-80`, `:148-199`). It is used by `println`, `print`, `String + value`, `join` and `as String`, and for the elements inside the list and map formats. It dispatches in this order:

1. `null`: `"null"`.
2. Arrays: array format; `char[]` becomes a string.
3. `Range`: `range.toString()`. That is `from..to` or `from..<to` (Groovy `groovy/lang/IntRange.java:415-420`). A `NumberRange` with a step other than 1 adds `.by(step)` (Groovy `NumberRange.java:545-558`). An empty range prints `x..<x` (Groovy `EmptyRange.java:95-99`). So `println(1..3)` prints `1..3`, and `'x' + (1..3)` is `x1..3`, while `"${1..3}"` is `[1, 2, 3]` (observed). A range nested inside a collection also uses this rule: `"${[1..3, 'a'..'b']}"` is `[1..3, a..b]` (observed).
4. `Collection`: list format.
5. `Map`: map format.
6. `CharSequence`: the text, without quotes.
7. Anything else: the Java `toString()` of the object.

### 6.2 Numbers

- `Integer`, `Long`, `BigInteger`: decimal digits, with `-` for negative values.
- `BigDecimal`: Java `BigDecimal.toString()` (Java behaviour, observed on all examples below).
  - Let `u` be the unscaled value and `s` the scale. Let `adjusted = -s + (number of digits of |u|) - 1`.
  - If `s >= 0` and `adjusted >= -6`, print plain notation with exactly `s` digits after the point (no point if `s == 0`).
  - Otherwise print scientific notation: the digits of `|u|` with a point after the first digit (if there is more than one digit), then `E`, then the sign (`+` or `-`), then `adjusted`. A negative scale always gives this form.
  - Observed: `1.50` is `1.50`, `100.0` is `100.0`, `1e3` is `1E+3`, `1.0e3` is `1.0E+3`, `1.59e7` is `1.59E+7`, `1e-7` and `0.0000001` are `1E-7`, `1e-3` is `0.001`, `1.0 * 3` is `3.0`, and `10 / 0.5` is `2E+1`.
- `Double`: Java `Double.toString` (Java behaviour, observed on JDK 21).
  - The digits are the shortest decimal that reads back as the same double.
  - If `1e-3 <= |x| < 1e7`, the layout is plain, with at least one digit after the point. Otherwise it is `d.dddE<exp>`, with at least one fraction digit and no `+` sign on the exponent.
  - `NaN`, `Infinity`, `-Infinity` and `-0.0` print as written.
  - Observed: `1.0`, `100.0`, `1.0E7`, `1.2345678E7`, `0.001`, `1.0E-4`, `0.30000000000000004`, `1.0E20`, `-0.0`.
  - Older JDKs used a different digit algorithm for some values (unverified).
- `Float`: Java `Float.toString`, the same layout with float precision (observed: `1.1f` is `1.1`, `1e10f` is `1.0E10`).
- A Rust evaluator can take the shortest round-trip digits of an `f64` (Rust's own float formatting also produces them; unverified). It must then apply the Java layout rules above.

### 6.3 Collections, maps and arrays

**List format** (Groovy `FormatHelper.java:278-298`): `[`, then each element in iteration order, formatted with `FormatHelper.toString` and separated by `, ` (comma and one space), then `]`. An empty collection is `[]`. An element that is the collection itself prints `(this Collection)`.

**Map format** (Groovy `FormatHelper.java:240-272`): `[`, then `key:value` pairs in iteration order, separated by `, `, then `]`. Keys and values use `FormatHelper.toString`. There is no space after `:` and no quoting. An empty map is `[:]`. A key or value that is the map itself prints `(this Map)`.

**Array format** is the same as the list format. A self-reference prints `(this array)` (Groovy `FormatHelper.java:439-466`).

Observed:

| Expression | Rendering |
| --- | --- |
| `"${[1, 'a', null, 1.50, 2d]}"` | `[1, a, null, 1.50, 2.0]` |
| `"${[1, [2, [3, [a: [4]]]]]}"` | `[1, [2, [3, [a:[4]]]]]` |
| `"${[id: 'S1', single_end: false, n: 2]}"` | `[id:S1, single_end:false, n:2]` |
| `"${[a: [1, 2], b: [c: 'd e'], (1): null]}"` | `[a:[1, 2], b:[c:d e], 1:null]` |
| `"${[]}"`, `"${[:]}"` | `[]`, `[:]` |
| `"${[1..3, 'a'..'b']}"` | `[1..3, a..b]` |
| `"${tuple('a', 1)}"`, `"${record(id: 'x', n: 1)}"` | `[a, 1]`, `[id:x, n:1]` |
| `"${[3, 1, 2] as Set}"` | `[3, 1, 2]` |
| `"${'x_y_z'.split('_')}"` | `[x, y, z]` |
| `"${''.split(',')}"` | `[]` (an array with one empty string) |
| `"${[a: 1, b: 2].keySet()}"`, `"${[a: 1, b: 2].values()}"` | `[a, b]`, `[1, 2]` |

Note that strings inside collections are not quoted. `['a b']` and `['a', 'b']` both contain the text `a b`, so the rendering alone cannot tell them apart.

### 6.4 Nextflow types

- **Path**: `Path.toString()`. `file()` returns an absolute, normalised path: `file('rel.txt')` is `<launchDir>/rel.txt`, `file('/tmp/a/../b.txt')` is `/tmp/b.txt`, and a trailing `/` is dropped (observed). For a remote path, the string has no scheme: `file('s3://some-bucket/hello.txt')` renders as `/some-bucket/hello.txt`. Use `toUriString()` to keep the scheme (`docs/reference/stdlib-types/path.mdx`, `toUriString`; `FilesEx.groovy:1608-1618`). Remote paths were not run (unverified beyond the documentation).
- **TaskPath** (a task input file): `toString()` is the staged name, for example `a b.fq` (`modules/nextflow/src/main/groovy/nextflow/processor/TaskPath.groovy:75-77`). In a task script it is escaped (section 6.7).
- **BlankSeparatedList** (several files for one `path` input): its own `toString()` joins the names with one space (`modules/nextflow/src/main/groovy/nextflow/util/BlankSeparatedList.groovy:48-50`). But it is a `Collection`. So outside the task script escaping rule, GString interpolation and `+` use the list format. Observed: `tag "${reads}"` gave the tag `[a b.fq, c.fq]`, an `exec:` block rendered `"${reads}"` as `[a b.fq, c.fq]`, `'x' + reads` was `x[a b.fq, c.fq]`, and `reads.toString()` was `a b.fq c.fq`.
- **MemoryUnit** (`MemoryUnit.groovy:137-150`):
  1. If `size <= 0`, the text is `0`.
  2. `groups = (int)(Math.log10(size) / Math.log10(1024))`.
  3. `value = size / 1024^groups` as a `double`.
  4. Format `value` with Java `DecimalFormat("0.#")`, decimal separator `.`, no grouping. `DecimalFormat` rounds `HALF_EVEN` on the exact binary value (Java behaviour, consistent with the observations below).
  5. Append a space and the unit from `B KB MB GB TB PB EB ZB`.

  Observed: `2.GB` is `2 GB`, `1536.MB` is `1.5 GB`, `1.25.GB` is `1.2 GB`, `1.35.GB` is `1.3 GB`, `1.95.GB` is `2 GB`, `1000.B` is `1000 B`, `1024.B` is `1 KB`, `1023.MB` is `1023 MB`, `0.B` is `0`, `1.EB` is `1 EB`, `1.ZB` is `8 EB`, and `2.GB / 3` is `682.7 MB`.

- **Duration** (`Duration.groovy:290-330`):
  1. If `ms < 1000`: `<ms>ms`.
  2. Else if `ms < 60000`: `Math.round(ms / 1000 * 10 as double) / 10` with Groovy division, then `s`. The result is a `BigDecimal` from an exact division by 10. So it has at most one decimal and no trailing `.0`.
  3. Else: `secs = Math.round((double)(ms / 1000))`, which rounds half up. Split into days, hours, minutes and seconds. Print the non-zero parts in the order `d h m s`, each as `<n><unit>`, joined by one space.

  Observed: `1.h` is `1h`, `90.min` is `1h 30m`, `1500.ms` is `1.5s`, `2.s` is `2s`, `1.25.s` is `1.3s`, `59999.ms` is `60s`, `59.5.s` is `59.5s`, `61.s` is `1m 1s`, `90.5.s` is `1m 31s`, `119999.ms` is `2m`, `60000.ms` is `1m`, `25.h` is `1d 1h`, `36.5.h` is `1d 12h 30m`, `0.ms` is `0ms`, and `1.5.ms` is `2ms`.

- **VersionNumber**: the input string with leading and trailing `.` and `-` removed (`VersionNumber.groovy:45-48`, `:131-133`). Observed: `"${nextflow.version}"` is `26.04.6`.
- **Record**: a `RecordMap` uses the map format (`[a:1]`), both in a GString and with `toString()` (observed).
- **Tuple**: an `ArrayTuple` uses the list format.

### 6.5 Other objects

- `Map.Entry`: Java `key=value` (observed: `"${[a: 1].entrySet().first()}"` is `a=1`; `map.find { ... }` returns an entry).
- `Boolean`: `true` / `false`.
- `Matcher`: Java `toString()`, for example `java.util.regex.Matcher[pattern=b region=0,3 lastmatch=]` (observed).
- `Pattern`: the pattern text (observed: `"${~/a.c/}"` is `a.c`).
- `Class`: `class java.lang.String` (observed).
- `GroupKey`: the wrapped key's `toString()`, called from dynamic Groovy (`modules/nextflow/src/main/groovy/nextflow/extension/GroupKey.groovy:89-91`). Observed: `"${groupKey('a', 2)}"` is `a`, and `"${groupKey([a: 1], 2)}"` is `[a:1]`.

### 6.6 Which function applies where

| Context | Function | Observed example |
| --- | --- | --- |
| `"...${v}..."` | `FormatHelper.write` | `"${1..3}"` is `[1, 2, 3]` |
| `println v`, `print v` | `FormatHelper.toString`, via Groovy `Script.println` (Groovy `groovy/lang/Script.java:171-182`, `DefaultGroovyMethods.java:864-866`), when ANSI logging is off (`modules/nextflow/src/main/groovy/nextflow/script/BaseScript.groovy:334-365`) | `println(1..3)` prints `1..3`; `println([a: 1])` prints `[a:1]` |
| `str + v` | `str + DefaultGroovyMethods.toString(v)` (Groovy `StringGroovyMethods.java:2150-2152`) | `'x' + [1, 2]` is `x[1, 2]`, `'x' + null` is `xnull` |
| `n + str` | `toString(n) + str` (Groovy `StringGroovyMethods.java:2163-2165`) | `1 + 'x'` is `1x` |
| `null + str` | `"null" + str` (Groovy `NullObject.java:172-174`) | `nullx` |
| `list.join(sep)` | `FormatHelper.toString` per element (Groovy `DefaultGroovyMethods.java:7004-7018`) | `[1, null, 'a', 1.50, [2, 3]].join(',')` is `1,null,a,1.50,[2, 3]` |
| `v.toString()` on a list or map | Groovy `toString` (list or map format). A class that declares its own `toString()` keeps it. | `[a: 1].toString()` is `[a:1]`; `(1..<1).toString()` is `1..<1`; a `BlankSeparatedList` gives `a b.fq c.fq` |
| `String.valueOf(v)`, `String.format('%s', v)` | Java `toString()` | `String.valueOf([a: 1])` is `{a=1}` |
| `v as String` | `FormatHelper.toString` (Groovy `DefaultGroovyMethods.java:18091-18094`) | `[1, 2] as String` is `[1, 2]` |

When ANSI logging is on, `print` and `println` send `object?.toString()` to the logger instead (`BaseScript.groovy:334-365`). `BaseScript` is `@CompileStatic`. Whether that call uses the Java or the Groovy `toString()` was not verified. A pseudo-terminal run with `-ansi-log true` still printed `[a:1]` for a map (observed), but it is not certain that ANSI logging was active (unverified).

`String.format` and `printf` use `java.util.Formatter`. `%.1f` of the `BigDecimal` `1.25` is `1.3` (half up, observed).

### 6.7 Task scripts

The `script:` (and `stub:`) section of a process is a closure that returns a string. Three extra rules apply.

**Escaping of interpolated values.** Each value inserted into a GString in these sections first goes through `LangHelpers.applyPathEscapeAware` (section 2 point 3). If the value implements `PathEscapeAware`, its `toStringEscape()` string is inserted instead of its normal rendering:

- A single input file is a `TaskPath`. Its escaped form is `Escape.path(stagedName)` (`TaskPath.groovy:458-460`).
- Several input files for one `path` input are a `BlankSeparatedList` of `TaskPath` (`modules/nextflow/src/main/groovy/nextflow/processor/TaskInputResolver.groovy:307-319`). Its escaped form is the escaped names joined by one space (`BlankSeparatedList.groovy:52-59`).
- `Escape.path` puts a backslash before each of these 14 characters: single quote, double quote, space, `(`, `)`, backslash, `!`, `&`, `|`, `<`, `>`, backtick, `:` and `;` (`modules/nf-commons/src/main/nextflow/util/Escape.groovy:32-60`). There is one exception. A `!` is a glob complement, and is not escaped, when three things hold: it directly follows a `[`, that `[` is not itself preceded by a backslash, and a `]` appears later in the name. Other characters are not escaped, including `$`, `*`, `?`, `{`, `}`, `[`, `]`, tab and newline.
- Only the direct value of `${...}` is checked. Observed: `${reads}`, `${reads[0]}` and a local copy `def r = reads; ${r}` are escaped. But `${reads.join(',')}` is a plain string and is not escaped.
- A `TaskPath` inside an ordinary list is not escaped, because the list is not `PathEscapeAware` (observed: `"${[reads[0]]}"` gave `[a b.fq]`).
- The rule covers every GString in the section, including GStrings assigned to local variables (observed).
- `exec:` and `shell:` sections are not transformed (`ProcessToGroovyVisitorV1.java:56-58`).

Excerpt of the observed `.command.sh` for inputs `a b.fq` and `c.fq` (one `path` input) and `d(1).fq` (another):

```bash
echo "reads=a\ b.fq c.fq"
echo "reads0=a\ b.fq"
echo "plain=a b.fq,c.fq"
echo "single=d\(1\).fq name=d(1).fq base=d(1) simple=d(1) ext=fq"
echo "meta=[id:S1, single_end:false]"
echo "opts=[1, two, [x:3]]"
echo "cpus=2 mem=6 GB giga=6 time=1h 30m attempt=1"
echo "bool=false num=3.0 div=3.5"
echo "$HOME and ${USER}"
echo "disk=null queue=null"
```

**Bash dollars.** A Bash `$VAR` must be written `\$VAR` or `\${VAR}` in a double-quoted script, because an unescaped `$` starts interpolation. Unset directive values render as `null` (observed above).

**Normalisation.** After the closure returns, `TaskProcessor.normalizeScript` normalises the string (`modules/nextflow/src/main/groovy/nextflow/processor/TaskProcessor.groovy:758-770`):

1. It runs `stripIndent(true)`, the Groovy algorithm (Groovy `StringGroovyMethods.java:2820-2873`):
   - Split the text into lines on LF, CR LF or CR (`LineIterable`, `StringGroovyMethods.java:603-616`).
   - Find the smallest number of leading whitespace characters over all lines that are not all whitespace. `Character.isWhitespace` decides, so a tab counts as one character.
   - If that number is 0, or there is no such line, the text is returned unchanged. Whitespace-only lines then keep their spaces.
   - Otherwise, remove that many characters from the start of each non-blank line. Turn each all-whitespace line into an empty line. Join the lines with LF. Keep a final LF only if the input ended with one.
2. `trim()`: remove leading and trailing whitespace of the whole script.
3. Append `\n`.
4. If the script does not start with `#!`, prepend the shebang line from the `shell` directive. That is `#!` plus the joined shell command if it starts with `/`, else `#!/usr/bin/env ` plus the command (`TaskProcessor.groovy:738-752`). Observed default: `#!/bin/bash -ue`.

Note that a user call to `str.stripIndent()` without an argument runs the **Java** `String.stripIndent()`, which is different. It also removes trailing spaces on each line. And a last line that is only whitespace counts when finding the indent. Observed: `"    a\n  ".stripIndent()` is `"  a\n"`, and `"  a  \n  b".stripIndent()` is `"a\nb"`.

