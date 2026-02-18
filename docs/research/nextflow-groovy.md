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

## 7. Groovy truth

A value is converted to a boolean in `if`, `assert`, `!`, `&&`, `||`, `?:`, the ternary condition, and `filter`-like closures. The rule is `DefaultTypeTransformation.castToBoolean` (Groovy `org/codehaus/groovy/runtime/typehandling/DefaultTypeTransformation.java:185-198`). `null` is false, and a `Boolean` is itself. Anything else calls the most specific `asBoolean()` method.

| Value | False when | Source |
| --- | --- | --- |
| `null` | always | Groovy `DefaultTypeTransformation.java:187` |
| Boolean | `false` | |
| Integer, Long, BigInteger, BigDecimal | `doubleValue() == 0` | Groovy `DefaultGroovyMethods.java:12136-12142` |
| Double, Float | zero or `NaN` | Groovy `DefaultGroovyMethods.java:12111-12126` (observed: `NaN` is false) |
| String (any `CharSequence`) | length 0 (`' '`, `'false'` and `'0'` are true) | Groovy `StringGroovyMethods.java:92-97` |
| Collection (List, Set, range, tuple) | empty (`[null]` is true) | Groovy `DefaultGroovyMethods.java:11874-11880` |
| Map, Record | empty | Groovy `DefaultGroovyMethods.java:11892-11898` |
| Array (`String[]`) | length 0 | Groovy `DefaultGroovyMethods.java:11943` |
| Iterator | no next element | Groovy `DefaultGroovyMethods.java:11909` |
| Matcher | `matcher.find(0)` is false; the test resets the matcher | Groovy `StringGroovyMethods.java:107-113` |
| MemoryUnit | zero bytes | `MemoryUnit.groovy:180-182` |
| Duration | zero milliseconds | `Duration.groovy:348-350` |
| Path, Closure, other objects | never (a Path to a missing file is true) | Groovy `DefaultGroovyMethods.java:11845` |

The documentation table (`docs/reference/semantics.mdx`, section "Booleans") agrees. It does not list `NaN`, matchers, arrays, MemoryUnit or Duration.

A consequence of the number rule: a `BigDecimal` so small that its `doubleValue()` is `0.0` is false (observed: `1e-400` is falsy).

## 8. Operators

### 8.1 Precedence and associativity

From highest to lowest (`modules/nf-lang/src/main/antlr/ScriptParser.g4:508-579`; `docs/reference/syntax.mdx`, section "Precedence"). Each alternative of the ANTLR `expression` rule is its own level, in the order written. So `as`, `instanceof` and the relational operators are three levels (`ScriptParser.g4:540-542`). The documentation lists them separately too.

| Level | Operators | Notes |
| --- | --- | --- |
| 0 | primary, `.x`, `?.x`, `*.x`, `(args)`, trailing closure, `[index]` | |
| 1 | `!`, `~` (prefix) | `!a == b` is `(!a) == b` (observed: `!1 == false` is true) |
| 2 | `**` | left-associative: `2 ** 3 ** 2` is 64 |
| 3 | unary `+`, `-` | `-2 ** 2` is `-4` (observed) |
| 4 | `*`, `/`, `%` | |
| 5 | binary `+`, `-` | |
| 6 | `<<`, `>>`, `>>>`, `..`, `..<` | `1 << 2 + 1` is 8; `1 + 1..3` is `[2, 3]` (observed) |
| 7 | `as` | `3 < 20 as String` is `3 < ('20')` and throws "Cannot compare" (observed) |
| 8 | `instanceof`, `!instanceof` | `1 < 2 instanceof Boolean` is `1 < (2 instanceof Boolean)` and throws (observed) |
| 9 | `<`, `>`, `<=`, `>=`, `in`, `!in` | `1 in [1] == true` is `true` |
| 10 | `==`, `!=`, `<=>` | |
| 11 | `=~`, `==~` | lower than `==`: `'a' == 'a' =~ /true/` is a `Matcher` (observed) |
| 12 | `&` | |
| 13 | `^` | |
| 14 | `\|` | |
| 15 | `&&` | short-circuit |
| 16 | `\|\|` | short-circuit |
| 17 | `? :`, `?:` | right-associative |

Assignment is a statement, not an expression (section 12).

### 8.2 `==` and `!=`

`a == b` runs `LangHelpers.compareEqual(a, b)` (`LangHelpers.java:47-88`). `a != b` is its negation. The steps are:

1. If `a` and `b` are the same object (or both `null`): true.
2. If both are `Integer`, or both `Long`, or both `Double`: Java `equals`. For `Double` this means `0.0d == -0.0d` is **false** and `NaN == NaN` is **true** (observed).
3. If both are `Path`: `TaskPath.equals` (`TaskPath.groovy:88-101`). A `TaskPath` equals a plain path when the plain path's file name is the staged name and the plain path equals the target. Two plain paths use Java `equals`. A `Path` never equals a `String`: `file('/tmp/a') == '/tmp/a'` is false (observed).
4. If either is a `MemoryUnit`: false if the other is `null`. Else `MemoryUnit.compareTo(unit, other) == 0`, where the other side may be a `MemoryUnit`, a `Number` (bytes, via `toLong()`) or a `CharSequence` (parsed) (`MemoryUnit.groovy:154-172`). `1.GB == '1 GB'` and `1.GB == 1073741824` are true (observed). **Zero values:** `MemoryUnit.compareTo` starts with `assert left` (`MemoryUnit.groovy:155`). That is Groovy truth, and a zero `MemoryUnit` is false. So when the `MemoryUnit` operand is zero bytes, the comparison throws a `PowerAssertionError`. That is an `Error`, which `catch( e )` does not catch. The `MemoryUnit` operand is the left one, or the right one when the left is not a `MemoryUnit`. Observed: `0.B == 0.B`, `0.B < 1.GB` and `0.ms == 0.ms` throw; `1.GB > 0.B` is `true`.
5. The same applies for `Duration`, with numbers in milliseconds and the same zero-value `assert` (`Duration.groovy:357-375`).
6. Otherwise Groovy `DefaultTypeTransformation.compareEqual` (Groovy `DefaultTypeTransformation.java:817-856`):
   - One side `null`: false.
   - If `a` is `Comparable`: `compareToWithEqualityCheck(a, b) == 0` (Groovy `DefaultTypeTransformation.java:749-811`):
     - Number and Number: numeric comparison after promotion (section 5.1). So `1 == 1L`, `1 == 1.0`, `1.0 == 1.00` and `0.1 == 0.1d` are true, and `0.1f == 0.1d` is false (observed). A `BigDecimal` and a `Double` are compared as doubles.
     - Number and a one-character String: the number is compared with the character code. `97 == 'a'` and `'a' == 97` are **true**; `1 == '1'` is false (observed).
     - String and String (or GString): `String.compareTo`.
     - Otherwise, `a.compareTo(b)` is tried when one class is assignable to the other, or when `b` is `Comparable`. A `ClassCastException` means "not equal". If neither holds, the result is "not equal". Observed: `'abc' == 5`, `5 == 'abc'`, `true == 'true'` and `[1] == '[1]'` are false.
   - Two arrays: same length and element-wise `compareEqual`.
   - An `Object[]` and a `List`, or two `List`s: same size and each pair equal by "coerced equality". Coerced equality compares numbers numerically, and otherwise uses `compareEqual` (Groovy `DefaultGroovyMethods.java:13576-13666`). So `[1, 2] == [1L, 2L]`, `[1, 2] == [1.0, 2.0]` and `['a', 'b'] == 'a,b'.split(',')` are true, and a tuple equals a list with the same elements (observed).
   - Two `Map.Entry`s: Java-equal keys and `compareEqual` values.
   - Otherwise `a.equals(b)`, dispatched by Groovy. So these Groovy overloads apply:
     - Two `Set`s: same size, and every element has a coerced-equal partner (Groovy `DefaultGroovyMethods.java:13694-13725`). A set never equals a list (observed).
     - Two `Map`s: same size, `keySet().equals(...)` (Java equality of keys), and coerced-equal values (Groovy `DefaultGroovyMethods.java:13738-13764`). Order does not matter. A record equals a map with the same entries (observed). `[1: 'a'] == [(1L): 'a']` is false (observed).
   - `GroupKey` defines `equals` as equality with its key (`GroupKey.groovy:79-82`). So `groupKey('a', 2) == 'a'` is true, but `'a' == groupKey('a', 2)` is false, because `String` is `Comparable` and takes the branch above (observed).

### 8.3 `<`, `<=`, `>`, `>=`

These run `LangHelpers.compareLessThan` and friends (`LangHelpers.java:98-262`):

1. Both `Integer`, both `Long` or both `Double`: primitive comparison.
2. A `MemoryUnit` or `Duration` on either side: `compareTo` as in 8.2 step 4, including the zero-value `assert`. But a `null` on the other side gives **false** for every operator (observed: `1.GB < null` is false).
3. Otherwise `ScriptBytecodeAdapter.compareTo(a, b)`, then the sign test. That is `DefaultTypeTransformation.compareTo`, the same algorithm as 8.2 step 6 but without the equality shortcut:
   - `null` is smaller than everything: `null < 1` is true, and `1 < null` is false (observed).
   - Numbers compare numerically after promotion.
   - Strings compare by `String.compareTo` (UTF-16 code units): `'10' < '9'` and `'B' < 'a'` are true (observed).
   - A Number and a one-character String compare by character code: `1 < '2'` is true (observed).
   - Anything that cannot be compared throws `IllegalArgumentException` ("Cannot compare ..."). Examples: `[1] < [2]` and `1 < 'ab'` (observed).
   - Paths compare with `Path.compareTo` (observed: `file('/tmp/a') < file('/tmp/b')`).

### 8.4 `<=>`

`a <=> b` is `ScriptBytecodeAdapter.compareTo(a, b)`, normalised to `-1`, `0` or `1` (Groovy `org/codehaus/groovy/runtime/ScriptBytecodeAdapter.java:773-780`). So `'a' <=> 'c'` is `-1`, while `'a'.compareTo('c')` is `-2` (observed). It is not rewritten to `LangHelpers`. So `MemoryUnit` and `Duration` use their own `compareTo(MemoryUnit)` or `compareTo(Duration)`. That has no zero-value `assert`: `0.B <=> 2.GB` is `-1`. But it does not accept strings or numbers: `1.GB <=> '1 GB'` throws "Cannot compare" (observed).

### 8.5 `in` and `!in`

`a in b` is `ScriptBytecodeAdapter.isCase(a, b)`, and `a !in b` is its negation (Groovy `ScriptBytecodeAdapter.java:714-723`). If `b` is `null`, the result is `a == null` (identity). Otherwise Groovy truth of `b.isCase(a)` is used. The method depends on the class of `b`:

| `b` | Test | Source and observed |
| --- | --- | --- |
| `null` | `a` is `null` | `ScriptBytecodeAdapter.java:715-717`; `null in null` is true (observed) |
| Collection (List, Set, tuple, `BlankSeparatedList`) | `b.contains(a)`, Java `equals` | Groovy `DefaultGroovyMethods.java:1196-1198`; `2L in [1, 2]` is **false**; `1.0 in [1.00]` is **false**; `null in [null]` is true |
| `IntRange` | `a` is an `Integer` or `BigInteger` between the ends | Groovy `IntRange.java:428-438`; `2 in 1..3` true, `2L in 1..3` **false**, `2.5 in 1..3` false |
| `NumberRange` (number ends, not both `Integer`) | walks the range elements and tests each with `compareEqual` | Groovy `NumberRange.java:566-577`; `2.5 in (1.5..3)` true, `2.0 in (1.5..3)` **false** (observed) |
| `ObjectRange` (for example strings) | walks the range elements and tests each with `compareEqual` | Groovy `ObjectRange.java:387-397`; `'b' in 'a'..'c'` true |
| Map, Record | Groovy truth of `b.get(a)` | Groovy `DefaultGroovyMethods.java:1217-1219`; `'a' in [a: 0]` and `'a' in [a: null]` are **false** |
| String (any `CharSequence`) | `b.toString().equals(a.toString())`; `null` gives false | Groovy `StringGroovyMethods.java:1619-1624`; `'b' in 'abc'` false; `1 in '1'` **true** (observed) |
| Pattern | the whole `a.toString()` matches; `null` gives false | Groovy `StringGroovyMethods.java:1645-1656`; `'abc' in ~/a.c/` true (observed) |
| Class | `b.isInstance(a)`; if `a` is a Class, `b.isAssignableFrom(a)` | Groovy `DefaultGroovyMethods.java:1170-1176`; `'abc' in String` true (observed) |
| Number, when `a` is a Number | numeric comparison is 0 | Groovy `DefaultGroovyMethods.java:1231-1233`; `1 in 1.0` true (observed) |
| Closure | Groovy truth of `b.call(a)` | Groovy `groovy/lang/Closure.java:398-403` |
| Array | as a collection (`contains`) | Groovy `DefaultGroovyMethods.java:1118-1123`; `'y' in 'x_y_z'.split('_')` true (observed) |
| other | `b.equals(a)` | Groovy `DefaultGroovyMethods.java:1118-1123` |

The map rule is documented in `docs/reference/stdlib-types/map.mdx` (`in`). But `docs/reference/semantics.mdx` ("Membership operators") says `in` tests for a key (section 13). Use `containsKey` to test for a key.

### 8.6 Regular expressions

- `a =~ b` returns a `java.util.regex.Matcher` (Groovy `org/codehaus/groovy/runtime/InvokerHelper.java:308-325`). `a` is converted with `FormatHelper.toString` unless it is a `String`. So a `Path` is matched by its path text, and `null` by the text `null` (observed: `null =~ /nu/` is truthy). If `b` is a `Pattern`, it is used. Otherwise `b` is converted to a string and compiled with Java `Pattern.compile`.
- As a boolean, the matcher means `find(0)`: the pattern occurs somewhere.
- `a ==~ b` returns a `Boolean`: the whole string matches (Groovy `InvokerHelper.java:333-345`). If either side is `null`, the result is `false` (observed).
- `~'text'` and `~/text/` create a `Pattern`.
- The dialect is Java `java.util.regex`. This includes `(?i)` flags (observed: `'ABC' ==~ /(?i)abc/` is true), `\d`, possessive quantifiers and named groups `(?<name>...)`. Slashy strings pass backslashes through unchanged, except `\/`, `\uHHHH` and backslash-newline (section 4.3). So `/\d+/` is the Java pattern `\d+`.
- Matcher indexing, `m[i]` (Groovy `StringGroovyMethods.java:1369-1387`):
  - It counts all matches from the start (`getCount`, `:1481-1488`).
  - If `i` is outside `-count..count-1`, it throws `IndexOutOfBoundsException`. A negative `i` counts from the end.
  - The element is the matched `String` when the pattern has no groups. Otherwise it is a `List` of the whole match followed by each group (`null` for a group that did not take part).
  - Observed: `('sample_R1_001.fastq.gz' =~ /(.+)_R(\d)_/)[0]` is `[sample_R1_, sample, 1]`, and `('a1b2' =~ /\d/)[1]` is `2`.
- `m.count` / `m.getCount()` is the number of matches, as an `Integer`. `m.size()` is the same number as a `Long` (Groovy `StringGroovyMethods.java:2701-2703`; observed).
- `String.replaceAll(regex, replacement)` and `replaceFirst` use Java rules: `$1` refers to a group, and `\` escapes (observed: `'sample_R1.fq'.replaceAll(/_R(\d)/, '-$1')` is `sample-1.fq`). `replaceAll(regex) { m -> ... }` with a closure is a Groovy extension, and the closure gets the match (observed).
- `String.split(regex)` is Java. The argument is a regex, and trailing empty strings are removed. `'a.b.c'.split('.')` is an empty array, and `'a,b,,'.split(',')` is `[a, b]` (observed). `split(regex, -1)` keeps them (observed: `[a, b, , ]`). The result is a `String[]`.
- `str - ~/regex/` removes the first match (Groovy `StringGroovyMethods.java:1914-1916`).

### 8.7 Logical operators, ternary, Elvis

- `!a` is `!truth(a)` and returns a `Boolean`.
- `a && b` and `a || b` convert both sides with Groovy truth, short-circuit, and always return a `Boolean` (observed: `'a' && 'b'` is `true`, not `'b'`).
- `c ? x : y` evaluates only the chosen branch.
- `a ?: b` returns `a` itself (not a Boolean) if `a` is truthy, else `b`. So `0 ?: 5` is 5, and `'' ?: 'x'` is `x` (observed). `task.ext.args ?: ''` is the common pattern.
- `x ?= y` is `x = x ?: y` (`docs/reference/semantics.mdx`, section "Assignments"; observed: `def x = 0; x ?= 7` gives 7). The target is always written back, even when `x` is truthy. Observed: `def r = record(a: 1); r.a ?= 2` throws `UnsupportedOperationException`.

### 8.8 Safe navigation and spread

- `a?.b` and `a?.m()` give `null` when `a` is `null` (`docs/reference/syntax.mdx`, "Property expression"; observed).
- `a.b` on `null` throws `NullPointerException` ("Cannot get property 'b' on null object"). `a.m()` on `null` throws "Cannot invoke method m() on null object" (observed).
- `a*.b` and `a*.m()` collect the property or call over the elements of `a` into a new `ArrayList`. If `a` is `null`, the result is `null`. A `null` element gives a `null` result element, not an error, because the parser marks spread access as safe too (`ScriptAstBuilder.java:1490-1497`; observed: `['a', null]*.size()` is `[1, null]`).
- The spread operator in list literals and argument lists (`[*a]`, `f(*a)`) and the map spread `[*:m]` are not allowed (section 12).

### 8.9 `as` casts

`as T` calls `asType`. Nextflow adds cases for `String`, `GString`, `Number` and `File` (`modules/nf-commons/src/main/nextflow/extension/Bolts.groovy:433-513`). Only hard casts exist. The soft cast `(T) x` is not allowed (`docs/strict-syntax.mdx`, "Type conversions").

| Source | `as` target | Result (observed unless noted) |
| --- | --- | --- |
| String | `Integer`, `Long` | Java parse after `trim()`: `' 42 ' as Integer` is 42; `'4.5' as Integer` throws `NumberFormatException` |
| String | `Double`, `Float`, `BigDecimal` | Java parse: `'1e3' as BigDecimal` is `1E+3` |
| String | `Boolean` | Groovy truth: `'false' as Boolean` is **true**, `'' as Boolean` is false |
| String | `List` | list of one-character strings: `'abc' as List` is `[a, b, c]` |
| String | `Duration`, `MemoryUnit` | parsed (section 4.5) |
| String | `Path` | `FileHelper.asPath` |
| String | `URL`, `URI` | `new URL(str)`, `URI.create(str)` (`Bolts.groovy:446-451`) |
| Number | `Duration`, `MemoryUnit` | from `longValue()` (millis or bytes): `3600000 as Duration` is `1h`, `1024 as MemoryUnit` is `1 KB`. A negative number fails the `assert`. |
| Number | `Integer`, `Long` | truncates toward zero |
| Number | `Boolean` | Groovy truth |
| any | `String` | `FormatHelper.toString`: `[1, 2] as String` is `[1, 2]`, `[a: 1] as String` is `[a:1]` |
| Collection | `Set` | the same object if it is already a `Set`; otherwise a new `LinkedHashSet` in source order (Groovy `DefaultGroovyMethods.java:12177-12187`; observed) |
| Collection | `List` | the same object if it is already a `List` (a tuple stays a tuple); otherwise a new `ArrayList` (Groovy `DefaultGroovyMethods.java:11829-11835`; observed: `(a as List).is(a)` is true) |
| `null` | any | `null` |

`String.toInteger()`, `toLong()`, `toDouble()`, `toFloat()` and `toBigDecimal()` parse after trimming (Groovy `StringGroovyMethods.java:3153-3160` and neighbours; observed: `' 42 '.toInteger()` is 42, and `'4.0'.toInteger()` throws). `toBoolean()` is true only for `true`, `y` or `1` after trimming, ignoring case (observed: `'yes'.toBoolean()` is false).

With static typing, a typed local declaration such as `def x: Integer = ...` keeps the type at run time, so Groovy's conversion to a typed variable applies (section 2 point 4, observed). Casting to a record type uses `TypeHelper.asType` (section 10.10).

### 8.10 `instanceof`

`a instanceof T` is the Java class test on the runtime class, with the classes of section 3. Observed: `1 instanceof Integer` is true, `1L instanceof Integer` false, `1.5 instanceof Double` false, `tuple(1) instanceof List` true, `record(a: 1) instanceof Map` true, and `(1..2) instanceof List` true. `a !instanceof T` is the negation. A test against a user record type is `TypeHelper.instanceofRecordType`. Then `a` must be a `RecordMap`, and every non-nullable field must be present and not `null` (`modules/nf-commons/src/main/nextflow/util/TypeHelper.groovy:232-247`).

### 8.11 Indexing

Lists (`getAt`/`putAt`, Groovy `DefaultGroovyMethods.java:8250-8258`, `:8351-8362`, `DefaultGroovyMethodsSupport.java:139-148`):

- `l[i]` with `i >= 0`: the element, or `null` if `i >= size`.
- `l[-k]`: element `size - k`. If `size - k < 0`, it throws `ArrayIndexOutOfBoundsException` (observed: `[10, 20, 30][-4]` fails).
- `l[a..b]`: a new list of those elements (Groovy `DefaultGroovyMethods.java:7947-7960`).
  - A reversed range gives reversed order: `[10, 20, 30][2..0]` is `[30, 20, 10]`.
  - Negative ends count from the end.
  - An end past the list throws `IndexOutOfBoundsException` (`l[0..5]` fails).
  - `l[0..<0]` is empty (observed).
  - On a tuple, the result is a plain `ArrayList` (observed).
- `l[i, j]`: a list of `l[i]` and `l[j]`, with `null` for positions past the end (observed: `l[0, 5]` is `[10, null]`, and `[10, 20][-1, 0]` is `[20, 10]`).
- `l[i] = v` with `i >= size` pads with `null`: `x = [1]; x[3] = 4` gives `[1, null, null, 4]`. Negative indexes count from the end. Tuples throw `UnsupportedOperationException` on assignment.
- `l.get(i)` is the Java method: out of range throws.

Strings (`'abc'[i]` and ranges, Groovy `StringGroovyMethods.java:1425-1450`):

- Indexing returns a **String** of one character, not a `char` (observed).
- Negative indexes count from the end.
- A positive index past the end throws `StringIndexOutOfBoundsException`. A negative index before the start throws `ArrayIndexOutOfBoundsException` (observed: `'abc'[5]` and `'abc'[-4]`).
- `s[1..3]` is a substring; a reversed range reverses it (observed: `'abcd'[3..1]` is `dcb`); `s[0, 2, 4]` concatenates characters (observed).
- The documentation says `[]` on a string returns `char` (`docs/reference/stdlib-types/string.mdx`). At run time it is a `String`.

Arrays (`String[]`): `a[i]` with a negative index counts from the end. An index past the end throws `ArrayIndexOutOfBoundsException`, not `null` (observed: `'x_y_z'.split('_')[5]` fails).

Maps: `m[k]` is `m.get(k)` (`null` when missing), and `m[k] = v` is `m.put(k, v)`. Keys use Java `equals` and `hashCode`, so `[1: 'a'][1L]` and `[1.0: 'a'][1.00]` are `null` (observed).

Matchers: section 8.6.

### 8.12 Property access

- **Maps**: `m.key` and `m.'key'` read `m.get('key')`, and missing keys give `null`. This also hides real methods: `m.class` and `m.empty` are `null` for a map without such keys. Use `m.getClass()` and `m.isEmpty()` (observed). `m.key = v` puts. Records work the same for reading and throw on writing.
- **Other objects**: `x.name` calls `getName()` (or `isName()` for booleans), and `x.name = v` calls `setName(v)`. Examples: `file.name`, `file.baseName`, `task.cpus`, `90.min.minutes` (`getMinutes()`), `2.GB.bytes`, `nextflow.version.major` (observed).
- **Lists**: a property that the list itself has is read from the list: `['ab', 'c'].size` is 2, `[].empty` is true, and `[1, 2].first` is 1 (the JDK 21 `List.getFirst()`) (observed). Any other property name is spread over the elements: `[[a: 1], [a: 2], [b: 3]].a` is `[1, 2, null]`. An element that is not a map and lacks the property makes it throw (observed).
- **Strings**: only real getters work. `'abc'.size` fails, because it is a method, not a property. `''.empty` is `true`, and `'ab'.bytes` works (observed).
- **Integers**: unknown properties fall through to the unit suffixes (section 4.5), else `MissingPropertyException`.

### 8.13 `+`, `-`, `*` and `<<` on non-numbers

| Expression | Result | Source / observed |
| --- | --- | --- |
| `str + x` | concatenation with `FormatHelper.toString(x)` | Groovy `StringGroovyMethods.java:2150` |
| `num + str` | `toString(num) + str` | `1 + 2 + 'a'` is `3a`, `'a' + 1 + 2` is `a12` |
| `str - x` | remove the first occurrence of `toString(x)` | `'a-b-c' - '-'` is `ab-c` (Groovy `StringGroovyMethods.java:1892-1902`) |
| `str - pattern` | remove the first match | `'file.fastq.gz' - ~/\.gz$/` is `file.fastq` |
| `str * n` | repeat; `n = 0` gives `''`; a negative `n` throws `IllegalArgumentException` | Groovy `StringGroovyMethods.java:1928-1935`; observed |
| `str << x` | a `StringBuffer` with both parts | `'a' << 'b'` is the StringBuffer `ab` |
| `list + x` | new list with `x` appended; if `x` is a collection, its elements are appended | `[10] + 40` and `[10] + [40]` both give `[10, 40]`; `[10] + [[40]]` gives `[10, [40]]` |
| `list - x` | new list without all elements equal to `x` (or to any element of a collection `x`); numbers are compared numerically | `[1, 2, 2, 3] - [2]` is `[1, 3]`; `[1, 2] - [2L]` is `[1]`; `[1.0] - [1.00]` is `[]`; `[1, 2, 2L, 3] - 2` is `[1, 3]` |
| `list * n` | repeated list | `[10, 20] * 2`; `[1] * 0` is `[]` |
| `list << x` | **mutates** the list and returns it | observed: returns the same object |
| `set + iterable`, `set - iterable` | new `LinkedHashSet` | observed |
| `map + map` | copy of the left map, then `putAll` of the right; an existing key keeps its position and gets the new value | `[b: 2] + [a: 1] + [b: 3]` is `[b:3, a:1]` (Groovy `DefaultGroovyMethods.java:8604-8608`) |
| `map - map` | remove entries whose key and value both match | `[a: 1, b: 2] - [a: 1]` is `[b:2]`; `- [a: 5]` removes nothing (Groovy `DefaultGroovyMethods.java:14159-14172`) |
| `map << map` | **mutates** the left map (`putAll`) | observed |
| `record + record` | new record, right side wins | `RecordMap.java:67-72`, observed |
| `record + map` | throws `UnsupportedOperationException` | observed |
| `tuple + list` | throws `UnsupportedOperationException` (the copy is a tuple) | observed |
| `path / str` | `path.resolve(str)`; an empty or null `str` gives `path` | `FilesEx.groovy:1406-1418` |
| `path + str` | a sibling path whose file name has `str` appended | `file('/tmp/dir/x.gz') + '.bai'` is `/tmp/dir/x.gz.bai` (`FilesEx.groovy:1389-1404`) |
| `path << str` | appends text to the file | `docs/reference/stdlib-types/path.mdx` |

### 8.14 Assignment

- `x op= y` is `x = x op y` (`docs/reference/semantics.mdx`, "Assignments"). It builds a new object: `def x = [1]; def y = x; x += [2]` leaves `y` as `[1]` (observed). `x /= 2` on an `Integer` gives a `BigDecimal`. `m.n += 1` and `l[0] += 1` read, compute and write back. If the old value is `null`, `+` fails ("Cannot execute null+1", observed; Groovy `NullObject.java:161-163`).
- `def (a, b) = expr` and `(a, b) = expr` take `expr[0]`, `expr[1]`, ... with the indexing rules of section 8.11.
  - For a list, missing positions become `null` and extra elements are ignored (observed: `def (a, b, c) = [1, 2]` gives `c == null`; `def (a, b) = [1, 2, 3]` gives `1/2`).
  - For a `String[]` or a `String` that is too short, the missing index throws (observed: `def (a, b, c) = 'x_y'.split('_')` throws `ArrayIndexOutOfBoundsException`, and `def (a, b, c) = 'xy'` throws `StringIndexOutOfBoundsException`).
  - A long enough string is indexed by character (`def (a, b) = 'xy'`).
- A declaration without a value is `null`.

## 9. Closures and functions

### 9.1 Parameters

- `{ a, b -> ... }` declares parameters. A parameter may have a default: `{ a, b = 5 -> a + b }` (observed: `.call(1)` is 6).
- `{ ... }` and `{ -> ... }` both have one optional parameter named `it`, with default `null` (section 2 point 7). `getMaximumNumberOfParameters()` is 1 for both (observed). So `{ -> 5 }.call(1)` returns 5, and `[1].each { -> ... }` runs (observed). This differs from plain Groovy, where `{ -> ... }` takes no arguments.
- Using `it` in the body is allowed, with a deprecation warning (`modules/nf-lang/src/main/java/nextflow/script/control/VariableScopeVisitor.java:824-871`; `docs/strict-syntax.mdx`, "Implicit closure parameter").
- A parameter may not reuse the name of a variable in an enclosing scope (lint error "`x` is already declared", observed).

### 9.2 Calling closures and argument adaptation

- Call a closure value with `c.call(args)`. A local closure variable cannot be called as `c(args)` in strict syntax: the checker reports "`c` is not defined" (observed with `nextflow lint`; `VariableScopeVisitor.java:782`). An expression followed by arguments in parentheses, such as `{ a -> a * 2 }(21)`, becomes `.call(...)` (`ScriptAstBuilder.java:1896-1911`; observed result 42).
- Groovy first looks for a closure `doCall` that accepts the arguments as given. Only if none matches, and there is exactly one argument that is a `java.util.List`, does it spread the list into separate arguments and try again (Groovy `org/codehaus/groovy/runtime/metaclass/ClosureMetaClass.java:265-272`). This is the destructuring used by `map { a, b -> }`:
  - A one-parameter closure always receives the whole list.
  - A closure that can also be called with one argument, because its other parameters have defaults, also receives the whole list. Observed: `{ a, b = 5 -> "${a}/${b}" }.call([1, 2])` is `[1, 2]/5`.
  - Otherwise the list is spread when its size is an allowed argument count. Observed: `{ a, b -> }` takes a 2-element list, and `{ a, b, c = 5 -> "${a}/${b}/${c}" }.call([1, 2])` is `1/2/5`.
  - Any other size, or a non-list argument, throws `MissingMethodException`. Observed for 2 parameters on a 3-element list, 3 parameters on a 2-element list, and 2 parameters on an `Integer`.
  - Tuples, ranges and other `List` classes are spread the same way. Arrays (`String[]`) and sets are not lists, so they are not spread (`ClosureMetaClass.java:266`; observed: both throw `MissingMethodException` for a 2-parameter closure).
- The same adaptation applies to script functions: `def f(a, b)` called as `f([1, 2])` gets `a = 1` and `b = 2` (observed; Groovy `groovy/lang/MetaClassImpl.java:1362-1369`).
- Calling a closure with more arguments than parameters (without a single list) throws `MissingMethodException`: `{ it }.call(1, 2)` fails (observed).
- Channel operators call the closure with one argument per item (for example `mapper.call(it)`, `modules/nextflow/src/main/groovy/nextflow/extension/MapOp.groovy:56`). So the same rules decide destructuring. Observed: `channel.of([1, 2]).map { a, b -> ... }` and `channel.of(tuple('s', 1)).map { id, n -> ... }` destructure.

### 9.3 Iterating maps

Groovy map iteration methods look at the closure's maximum parameter count (Groovy `DefaultGroovyMethods.java:6059-6082`):

- `each`, `collect`, `findAll`, `find`, `any`, `every`, `collectEntries`, `groupBy` and similar: a closure with two parameters receives `(key, value)`. Otherwise the closure receives the `Map.Entry` (with `.key` and `.value`).
- `eachWithIndex`: three parameters receive `(key, value, index)`, and two receive `(entry, index)` (Groovy `DefaultGroovyMethods.java:6075-6082`).
- `inject(init) { acc, k, v -> }` with three parameters receives key and value. With two it receives the entry (Groovy `DefaultGroovyMethods.java:6185-6195`; observed).
- `findAll` on a map returns a map, `find` returns a `Map.Entry`, and `collect` returns a list (observed).

### 9.4 Return values

- A closure or function returns the value of its last statement, or of an explicit `return`. Groovy's `ReturnAdder` decides this (Groovy `org/codehaus/groovy/classgen/ReturnAdder.java`).
- `return` inside a closure only leaves that closure. In `each` it acts like "continue" (observed).
- An `if`/`else` as the last statement returns the value of the branch that ran (observed: `def fif(x) { if( x ) { 'yes' } else { 'no' } }` returns `'no'` for 0). If no branch runs, the result is `null` (observed: an `if` without `else` returned `null` for 0). A `try`/`catch` as the last statement returns the value of the block that finished (`ReturnAdder.java`).
- A `def x = 1` declaration as the last statement returns 1 (observed). `docs/reference/semantics.mdx` ("Returns") says that such a function returns nothing. The runtime disagrees.
- The type checker requires consistent returns (`docs/reference/semantics.mdx`, "Returns").

### 9.5 Captured variables

A closure refers to the variables of its enclosing scope, not to copies. A later reassignment is visible (observed: `def n = 10; def c = { v -> v + n }; n = 20; c.call(1)` is 21). The closure can also modify them (`total += v` inside `each` works, observed). Inside a workflow, the checker warns about writing to an outer variable inside an operator closure (`VariableScopeVisitor.java:703-712`).

### 9.6 Functions

- `def name(params) { body }`, optionally with `name: Type` parameters and a `-> Type` return type (`docs/reference/syntax.mdx`, "Function").
- Typed parameters keep their Java type at run time, except user record types (section 2 point 4). A call whose argument has another class fails with `MissingMethodException`, with no conversion (observed: `def typed(x: Integer)` called with `5L`).
- Default parameter values are allowed and evaluated per call (`docs/reference/semantics.mdx`, "Default parameter values").
- Named arguments are collected into one `Map`, passed as the **first** argument, wherever they appear in the call: `fmap(1, k: 'v')` and `fmap(k: 'v', 1)` both call `fmap([k: 'v'], 1)` (observed; `docs/reference/semantics.mdx`, "Named arguments").
- Arguments are passed by reference. Reassigning a parameter does not affect the caller (`docs/reference/semantics.mdx`, "Parameters").
- A function name cannot be `main`, `run` or `runScript`. The same applies to process, workflow and agent names (`ScriptToGroovyVisitor.java:72`, `:236-239`).

### 9.7 Delegate and owner

In ordinary script code, the checker resolves names statically. A name that is not a local variable, parameter, function or built-in is an error ("`foo` is not defined", observed). So an evaluator can resolve most names at compile time. Dynamic resolution matters in two places.

**Process directive closures.** Examples: `cpus { 2 * task.attempt }`, `memory { 6.GB * task.attempt }`, or `tag "${meta.id}"` (wrapped in a closure by the compiler, section 2 point 6). Such a directive is stored unevaluated. For each task, it is cloned with the task context as delegate and resolve strategy `DELEGATE_FIRST` (`TaskConfig.groovy:792-807`, `Bolts.groovy:697-706`). In a legacy process, the directives `executor`, `label`, `maxForks`, `module`, `pod`, `publishDir` and `secret` are not wrapped, and neither are directives with more than one argument (`ProcessToGroovyVisitorV1.java:79-105`). A typed process only skips directives with more than one argument (`ProcessToGroovyVisitorV2.java:101-111`).

**Config closures.** A closure assigned in a config file, such as `ext.args = { "--id ${meta.id} --n ${task.cpus}" }` inside `withName`, is stored as a closure (`modules/nextflow/src/main/groovy/nextflow/config/parser/v2/ConfigDsl.groovy:145-153`). It is resolved in the same way when the task reads it. Observed: the closure above rendered `--id S1 --n 2`, with `meta` and `task.cpus` from the task. `task.ext` itself stays a lazy map, and each read of `task.ext.x` resolves the stored value again (`TaskConfig.groovy:691-850`).

**Name lookup in the task context** (`TaskContext.get0`, `modules/nextflow/src/main/groovy/nextflow/processor/TaskContext.groovy:132-147`):

1. The task's own variables: process inputs, `task` (the `TaskConfig`), and variables assigned in the script section without `def`.
2. The script binding: `params`, `projectDir`, `launchDir`, `moduleDir`, `workDir`, `baseDir`, `workflow`, `nextflow`, `secrets`.
3. Otherwise `MissingPropertyException`: "Unknown variable 'x' -- Make sure it is not misspelled or defined later in the script".

Method calls from such a closure go to the script (`TaskContext.invokeMethod`, `:163-168`), except `template(...)`.

Local variables that the closure captures from its enclosing code are read directly, and the delegate does not affect them. Only names that are not local variables go through the delegate-first lookup above (standard Groovy closure semantics; unverified in the Groovy source).

## 10. Standard library

The methods below are the ones Nextflow documents and types for strict syntax. They come from:

- the typed shims in `modules/nf-lang/src/main/java/nextflow/script/types/shim/*.java`;
- the types in `modules/nf-lang/src/main/java/nextflow/script/types/*.java`;
- `docs/reference/stdlib-types/*.mdx`.

The runtime is the JDK, Groovy's extension methods, and Nextflow's `Bolts` and `FilesEx`. So extra methods also work (section 10.12).

### 10.1 String

Shim: `types/shim/String.java`. Docs: `docs/reference/stdlib-types/string.mdx`.

| Method or operator | Meaning (runtime notes in brackets) |
| --- | --- |
| `+` | concatenation (section 8.13) |
| `*` (Integer) | repeat [a negative count throws] |
| `[]` (Integer) | one-character `String` [not `char`, section 8.11] |
| `~str` | compile to `Pattern` |
| `=~`, `==~` | regex find / full match (section 8.6) |
| `in` (`isCase`) | equality of the `toString()` texts (section 8.5) |
| `compareTo` | lexical order [UTF-16 code units] |
| `contains(s)` | substring test |
| `endsWith(s)`, `startsWith(s)` | suffix / prefix test |
| `execute()` | run as an external command, returns `java.lang.Process` [observed: `'echo hi'.execute().text` is `hi\n`] |
| `indexOf(s)`, `indexOf(s, from)` | first index or -1 |
| `lastIndexOf(s)`, `lastIndexOf(s, from)` | last index or -1 |
| `isBlank()` | empty or only whitespace |
| `isEmpty()` | length 0 |
| `isDouble()`, `isFloat()` | parses as a double / float [observed: `'NaN'`, `'1'` and `'1e3'` count as doubles] |
| `isInteger()`, `isLong()` | parses as a 32 / 64-bit integer [after trim: `' 42'` is an integer; `'99999999999'` is not, observed] |
| `length()` | length in UTF-16 code units |
| `md5()`, `sha256()` | lowercase hex digest of the UTF-8 bytes [Groovy `EncodingGroovyMethods.java:377-379`, `:397-399`, `:419-438`; observed: `'abc'.md5()` is `900150983cd24fb0d6963f7d28e17f72`] |
| `readLines()` | lines split on `\n`, `\r\n` or `\r` [observed: `'a\nb\r\nc\n'` gives `[a, b, c]`] |
| `replace(t, r)` | replace every literal occurrence |
| `replaceAll(regex, r)`, `replaceFirst(regex, r)` | Java regex replacement (section 8.6) |
| `reverse()` | reversed string |
| `strip()`, `stripLeading()`, `stripTrailing()` | remove whitespace [Java, or `Bolts.strip` (`Bolts.groovy:178-180`); both use `Character.isWhitespace`] |
| `stripIndent()` | remove common indentation [Java `String.stripIndent`, section 6.7] |
| `substring(b)`, `substring(b, e)` | Java substring |
| `toBoolean()` | `true` for `true`, `y`, `1` after trim, any case |
| `toDouble()`, `toFloat()`, `toInteger()`, `toLong()` | parse after trim (section 8.9) |
| `toLowerCase()`, `toUpperCase()` | Java case conversion [locale unverified] |
| `tokenize()` | split on whitespace (`java.util.StringTokenizer`), drop empty tokens [Groovy `StringGroovyMethods.java:3168-3170`] |
| `tokenize(chars)` | split on any character in `chars`, drop empty tokens [observed: `'a,b,,c'.tokenize(',')` is `[a, b, c]`] |
| `trim()` | remove leading and trailing characters `<= ' '` [Java] |

### 10.2 Iterable (List, Set, Bag)

Shim: `types/shim/Iterable.java`. Docs: `docs/reference/stdlib-types/iterable.mdx`. All of these are Groovy `DefaultGroovyMethods` extension methods unless noted.

| Method | Meaning (runtime notes in brackets) |
| --- | --- |
| `any(pred)` | true if some element is truthy under `pred` [empty: false] |
| `collect(f)` | new `ArrayList` of `f(e)` |
| `collectMany(f)` | concatenation of the collections returned by `f` |
| `contains(v)` | Java `equals` membership [`[1, 2].contains(2L)` is false, observed] |
| `containsAll(vs)` | Java `containsAll` |
| `each(f)` | call `f` per element, return the receiver |
| `every(pred)` | true if all elements pass [empty: true] |
| `find(pred)` | first passing element or `null` |
| `findAll(pred)` | new list of passing elements; with no closure, the truthy elements [observed: `[0, 1, '', 'a', null, []].findAll()` is `[1, a]`] |
| `groupBy(f)` | `LinkedHashMap` from key to list, keys in first-seen order |
| `inject(f)` | fold with the first element as the start [empty: `NoSuchElementException`, observed] |
| `inject(init, f)` | fold from `init` |
| `intersect(other)` | elements of the receiver that are also in `other`, found with a `TreeSet` and the number-aware comparator [order of the receiver; observed `[3, 1, 2].intersect([2, 3])` is `[3, 2]`; Groovy `DefaultGroovyMethods.java:13202-13230`] |
| `isEmpty()` | no elements |
| `join()`, `join(sep)` | `FormatHelper.toString` of each element, joined [the default separator is empty; observed: `[1, 2].join()` is `12`] |
| `max()`, `min()` | largest / smallest by Groovy `>` / `<` (`ScriptBytecodeAdapter.compareGreaterThan` / `compareLessThan`), skipping `null` elements [empty: `null`; elements that cannot be compared throw; Groovy `DefaultGroovyMethods.java:7182-7191`, `:7525-7534`; observed: `[null, 3, 1].max()` is 3, and `[1, 'a'].max()` is `a`, because a one-character string compares by character code] |
| `max(f)`, `min(f)` | one-parameter closure: compare `f(e)`; two-parameter closure: comparator |
| `size()` | number of elements |
| `sum()` | `e0 + e1 + ...` with Groovy `+` [empty: `null`; strings concatenate (observed: `['a', 'b'].sum()` is `ab`); lists concatenate] |
| `sum(f)` | sum of `f(e)` |
| `toList()` | new `ArrayList` |
| `toSet()` | new `HashSet` [hash order, section 10.4] |
| `toSorted()` | new sorted list, stable, number-aware comparator (below) |
| `toSorted(f)` | one-parameter closure: sort by `f(e)`; two-parameter: comparator |
| `toUnique()` | new collection without duplicates, first occurrence kept [duplicates are found with a `TreeSet` in Java natural order (Groovy `DefaultGroovyMethods.java:2013-2015`, `:1830-1838`), so all elements must be mutually comparable in Java. Observed: `['b', 'a', 'b'].toUnique()` is `[b, a]`; `[1, 1L].toUnique()` and `[[a: 1]].toUnique()` throw `ClassCastException`; `[null, 1].toUnique()` throws `NullPointerException`] |
| `toUnique(f)` | as `toUnique()`, with `f` deciding equality |

The default comparator of `toSorted()` and `sort()` is Groovy's `NumberAwareComparator` (Groovy `org/codehaus/groovy/runtime/NumberAwareComparator.java:48-77`). It uses the `<=>` rules of 8.3: numbers numerically, strings by code unit, `null` first. If two elements cannot be compared, it falls back to comparing their `hashCode()` values. That gives an order that depends on Java hash codes (for example, maps sorted without a key closure). Observed: `['b', 'A', 'a', 'B', '10', '9'].toSorted()` is `[10, 9, A, B, a, b]`, `[2, null, 1].toSorted()` is `[null, 1, 2]`, and `[3, 1.5, 2L, 0.5d].toSorted()` is `[0.5, 1.5, 2, 3]`.

### 10.3 List

Shim: `types/shim/List.java`. Docs: `docs/reference/stdlib-types/list.mdx`.

| Method or operator | Meaning (runtime notes in brackets) |
| --- | --- |
| `+`, `*`, `[]`, `in`, `putAt`, `compareTo` | sections 8.5, 8.11, 8.13 [lists are not comparable at run time: `[1] < [2]` throws] |
| `collate(n)`, `collate(n, keepRemainder)` | sub-lists of size `n` |
| `collate(n, step)`, `collate(n, step, keepRemainder)` | sliding windows [observed: `[1, 2, 3, 4].collate(3, 1, false)` is `[[1, 2, 3], [2, 3, 4]]`; Groovy `DefaultGroovyMethods.java:3519-3541`] |
| `first()`, `head()` | first element [empty: `NoSuchElementException`; Groovy `DefaultGroovyMethods.java:10341-10346`] |
| `getIndices()` | `0..<size` as an `IntRange` |
| `indexOf(v)` | Java `indexOf` (Java `equals`) |
| `init()` | all but the last [empty: `NoSuchElementException`; Groovy `DefaultGroovyMethods.java:10577-10590`] |
| `last()` | last element [empty: `NoSuchElementException`] |
| `reverse()` | new reversed list [does not mutate] |
| `subList(from, to)` | Java view `[from, to)` |
| `tail()` | all but the first [empty: `NoSuchElementException`; Groovy `DefaultGroovyMethods.java:10503-10510`] |
| `take(n)` | first `n` elements [fewer if the list is shorter] |
| `takeWhile(pred)` | longest passing prefix |
| `withIndex()` | list of `[element, index]` pairs |

Mutating methods that real scripts call, but that are not in the shim: `add`, `addAll`, `remove`, `removeIf`, `clear`, `<<`, `sort()` and `unique()`.

- Groovy `sort()` and `unique()` **sort or de-duplicate a list in place** and return it (observed). `toSorted()` and `toUnique()` return new lists. On a tuple, `sort()` also works in place (section 3).
- `unique()` finds duplicates with Groovy coerced equality: numbers compare numerically, then `compareEqual` applies (Groovy `DefaultGroovyMethods.java:1340-1369`). So `[1, 1L, 1.0, 2].unique()` is `[1, 2]`, and `[[a: 1], [a: 1L]].unique()` is `[[a:1]]` (observed).

### 10.4 Set and Bag

Shims: `types/shim/Set.java`, `types/shim/Bag.java`. Docs: `docs/reference/stdlib-types/set.mdx`, `bag.mdx`.

- Set: `+` and `-` with an iterable (new set), `in`, `intersect(iterable)`.
- Bag: `+` (concatenation), `in`.

Iteration order matters, because sets are rendered and joined:

- `as Set` on a non-set collection, set `+` and set `-` produce a `LinkedHashSet` in insertion order (Groovy `DefaultGroovyMethods.java:12184-12187`, `DefaultGroovyMethodsSupport.java:275-291`; observed).
- `toSet()` produces `new HashSet(size)`, filled in order (Groovy `DefaultGroovyMethods.java:15382-15386`). Its iteration order is Java `HashMap` bucket order. To reproduce it:
  - The table capacity is the smallest power of two `>= size`, and at least 1.
  - The table doubles whenever the count exceeds `0.75 * capacity`. It keeps the relative order within the split buckets.
  - An element with hash code `h` goes to bucket `(h ^ (h >>> 16)) & (capacity - 1)`.
  - Iteration is by bucket index, then by insertion order within a bucket.
  - Hash codes: an `Integer` is its value. A `String` is `s[0]*31^(n-1) + ... + s[n-1]` in 32-bit arithmetic.
  - This model matches the observed orders: `[3, 1, 3, 2, 10, 100, -1].toSet()` gives `[-1, 1, 2, 10, 3, 100]` (observed again in this review), and `['sample', 'b', 'a', 'zz', 'aa'].toSet()` gives `[zz, aa, a, b, sample]`.
  - The resize and tree-bin details are Java behaviour. They were not checked against the JDK source (unverified).
- `docs/reference/stdlib-types/iterable.mdx` warns that converting an unordered collection to a list is not deterministic.

### 10.5 Map and Map.Entry

Shim: `types/shim/Map.java`. Docs: `docs/reference/stdlib-types/map.mdx`.

| Method or operator | Meaning (runtime notes in brackets) |
| --- | --- |
| `+` | section 8.13 |
| `[]` (`getAt`) | `get(k)` |
| `in` | Groovy truth of the value (section 8.5) |
| `any(f)`, `every(f)`, `each(f)` | section 9.3 |
| `containsKey(k)` | key present, even with a `null` value |
| `containsValue(v)` | Java `equals` [`[a: 2].containsValue(2L)` is false, observed] |
| `entrySet()` | entries in map order |
| `isEmpty()` | no entries |
| `keySet()` | keys in map order |
| `size()` | number of entries |
| `subMap(keys)` | new `LinkedHashMap` of the listed keys that exist, in the order of `keys` [missing keys are skipped; Groovy `DefaultGroovyMethods.java:8110-8118`; observed; on a record it returns a record] |
| `values()` | values in map order (a `Bag`) |
| `Entry.getKey()`, `Entry.getValue()` | key and value (`e.key`, `e.value`) |

Groovy methods that scripts use and that behave in surprising ways:

- `m.get(k, default)` **inserts** `default` when `k` is missing (Groovy `DefaultGroovyMethods.java:8167-8172`; observed). On a record it throws `UnsupportedOperationException` (observed). `getOrDefault` does not insert.
- `m.sort()` returns a `TreeMap` sorted by key in Java natural order (Groovy `DefaultGroovyMethods.java:9568-9570`; observed).
- `m.clone()` is a shallow copy (observed).
- `collectEntries`, `findAll`, `groupBy` and `countBy` return `LinkedHashMap`s.

