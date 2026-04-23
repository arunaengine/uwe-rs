# Writes run_corpus_launches.txt: how the run corpus test launches each workflow of the
# Snakemake corpus. Run it from the repository root after `just snakemake-corpus-data`:
#
#     uv run -q --with pyyaml --with snakemake==9.27.0 python snakemake-runtime/tests/fixtures/launches.py
#
# A catalog workflow runs plainly in its repository, unless real Snakemake cannot evaluate it
# that way (run_corpus_oracle.txt from oracle.py says so) and it can with another launch: the
# way its CI tests it, or with the mandatory flags of its catalog entry. The CI launch is the
# first step of its GitHub workflows that runs Snakemake on it, with the
# snakemake-github-action or a `snakemake` command, other than linting, reports, unit test
# generation and environment setup, and whose folders and files exist. The catalog launch
# takes the config files and values of the mandatory flags. A test of Snakemake runs the way
# the first test function of tests.py that uses its folder runs it, preferring runs that
# should not fail.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import ast
import contextlib
import glob
import io
import os
import shlex
import sys

import yaml
from snakemake.cli import get_argument_parser

CORPUS = os.environ.get("SNAKEMAKE_CORPUS") or os.path.expanduser("~/.cache/iunctus/corpus/snakemake")
FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "run_corpus_launches.txt")
ORACLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "run_corpus_oracle.txt")

# The parser of Snakemake's command line, and what its options that do not run the workflow
# set.
PARSER = get_argument_parser()
SKIPPED = [
    "lint", "report", "generate_unit_tests", "conda_create_envs_only", "containerize",
    "archive", "unlock", "list_rules", "list_target_rules", "dag", "rulegraph", "filegraph",
    "d3dag", "summary", "detailed_summary", "cleanup_metadata", "touch", "delete_all_output",
    "delete_temp_output", "print_compilation", "export_cwl", "list_changes",
    "list_input_changes", "list_params_changes", "list_untracked", "cleanup_shadow",
    "list_conda_envs", "conda_cleanup_envs", "edit_notebook",
]


def parse_command(words, base=""):
    """The launch fields of a Snakemake command line; `None` for one that does not run the
    workflow or that Snakemake cannot parse."""
    try:
        with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            arguments, _ = PARSER.parse_known_args(words)
    except (SystemExit, ValueError):
        return None
    if any(getattr(arguments, name, None) not in (None, False) for name in SKIPPED):
        return None
    fields = []
    if arguments.directory:
        fields.append(("directory", os.path.normpath(os.path.join(base, str(arguments.directory)))))
    if arguments.snakefile:
        fields.append(("snakefile", os.path.normpath(os.path.join(base, str(arguments.snakefile)))))
    for configfile in arguments.configfile or []:
        fields.append(("configfile", os.path.normpath(os.path.join(base, str(configfile)))))
    # Config values as written, which the options parse like the words after them.
    for index, word in enumerate(words):
        if word in ("--config", "-C"):
            for value in words[index + 1:]:
                if value.startswith("-"):
                    break
                if "=" in value:
                    fields.append(("config", value))
    targets = arguments.targets or set()
    fields += [("target", word) for word in dict.fromkeys(words) if word in targets]
    return fields


def command_words(text):
    """The words of the first command of a shell line, before `&&`, `|`, `;` or a
    redirection; `None` when the quotes do not match."""
    lexer = shlex.shlex(text, posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    words = []
    try:
        for word in lexer:
            if word and set(word) <= set("();<>|&"):
                if word[0] in "<>" and words and words[-1].isdigit():
                    words.pop()
                break
            words.append(word)
    except ValueError:
        return None
    return words


def exists(folder, fields):
    """Whether the working directory, the Snakefile and the config files of a launch exist."""
    values = dict(fields)
    directory = os.path.join(folder, values.get("directory", "."))
    if not os.path.isdir(directory):
        return False
    if "snakefile" in values:
        if not os.path.isfile(os.path.join(folder, values["snakefile"])):
            return False
    elif not any(os.path.isfile(os.path.join(directory, name)) for name in
                 ("Snakefile", "snakefile", "workflow/Snakefile", "workflow/snakefile")):
        return False
    return all(os.path.isfile(os.path.join(folder, value)) for key, value in fields
               if key == "configfile")


def github_launch(folder):
    """The launch of the first CI step that runs the workflow with files that exist, or
    `None`."""
    for path in sorted(glob.glob(os.path.join(folder, ".github/workflows/*.y*ml"))):
        try:
            document = yaml.safe_load(open(path, encoding="utf-8"))
        except (yaml.YAMLError, UnicodeDecodeError):
            continue
        if not isinstance(document, dict):
            continue
        for job in (document.get("jobs") or {}).values():
            for step in (job or {}).get("steps") or []:
                if not isinstance(step, dict):
                    continue
                uses = str(step.get("uses") or "")
                if "snakemake-github-action" in uses:
                    settings = step.get("with") or {}
                    arguments = str(settings.get("args") or "")
                    if "${{" in arguments:
                        continue
                    words = command_words(arguments)
                    if words is None:
                        continue
                    words = [
                        "--directory", str(settings.get("directory") or "."),
                        "--snakefile", str(settings.get("snakefile") or "Snakefile"),
                    ] + words
                    fields = parse_command(words)
                    if fields is not None and exists(folder, fields):
                        return fields
                command = str(step.get("run") or "").replace("\\\n", " ")
                for line in command.splitlines():
                    line = line.strip()
                    if not line.startswith("snakemake ") or "${{" in line:
                        continue
                    words = command_words(line)
                    if not words:
                        continue
                    fields = parse_command(words[1:])
                    if fields is not None and exists(folder, fields):
                        return fields
    return None


def catalog_launch(folder):
    """The config files and values among the mandatory flags of the catalog entry."""
    try:
        entry = yaml.safe_load(open(os.path.join(folder, ".snakemake-workflow-catalog.yml"), encoding="utf-8"))
        flags = entry["usage"]["mandatory-flags"]["flags"]
    except (OSError, yaml.YAMLError, KeyError, TypeError):
        return None
    if not isinstance(flags, str):
        return None
    fields = parse_command(command_words(flags) or []) or []
    fields = [field for field in fields if field[0] in ("configfile", "config")]
    return fields if fields and exists(folder, fields) else None


def constant(node):
    try:
        return ast.literal_eval(node)
    except ValueError:
        return None


def test_path(node):
    """The folder a `dpath("...")` argument names, relative to the tests folder."""
    if isinstance(node, ast.Call) and getattr(node.func, "id", None) == "dpath" and node.args:
        return test_path(node.args[0])
    if isinstance(node, ast.Call) and getattr(node.func, "id", None) == "Path" and node.args:
        return test_path(node.args[0])
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def tests_launches(tests):
    """The launch of each Snakemake test folder from the `run(dpath(...))` calls of tests.py."""
    launches = {}
    tree = ast.parse(open(os.path.join(tests, "tests.py"), encoding="utf-8").read())
    for function in tree.body:
        if not isinstance(function, ast.FunctionDef):
            continue
        for call in ast.walk(function):
            if not (isinstance(call, ast.Call) and getattr(call.func, "id", None) == "run" and call.args):
                continue
            path = test_path(call.args[0])
            if not path:
                continue
            path = os.path.normpath(path)
            folder, _, inside = path.partition("/")
            keywords = {keyword.arg: keyword.value for keyword in call.keywords if keyword.arg}
            shouldfail = keywords.get("shouldfail")
            failing = shouldfail is not None and constant(shouldfail) is not False
            fields = []
            if inside:
                fields.append(("directory", inside))
            snakefile = constant(keywords["snakefile"]) if "snakefile" in keywords else None
            if isinstance(snakefile, str):
                fields.append(("snakefile", os.path.normpath(os.path.join(inside, snakefile))))
            elif inside:
                fields.append(("snakefile", os.path.join(inside, "Snakefile")))
            shellcmd = constant(keywords["shellcmd"]) if "shellcmd" in keywords else None
            if isinstance(shellcmd, str):
                words = command_words(shellcmd)
                parsed = parse_command(words[1:], inside) if words else None
                if parsed is None:
                    continue
                fields = [field for field in fields if field[0] != "snakefile" or not any(
                    other[0] == "snakefile" for other in parsed)] + parsed
            targets = constant(keywords["targets"]) if "targets" in keywords else None
            if isinstance(targets, (list, set, tuple)):
                ordered = sorted(targets, key=str) if isinstance(targets, set) else list(targets)
                fields += [("target", str(target)) for target in ordered]
            config = constant(keywords["config"]) if "config" in keywords else None
            if isinstance(config, dict):
                fields += [("config", f"{key}={value}") for key, value in config.items()]
            configfiles = keywords.get("configfiles")
            if isinstance(configfiles, (ast.List, ast.Tuple)):
                for element in configfiles.elts:
                    configfile = test_path(element)
                    if configfile:
                        configfile = os.path.normpath(configfile)
                        if configfile.startswith(folder + "/"):
                            configfile = configfile[len(folder) + 1:]
                        fields.append(("configfile", configfile))
            if failing:
                fields.append(("shouldfail", ""))
            known = launches.get(folder)
            if known is None or (("shouldfail", "") in known and not failing):
                launches[folder] = fields
    return launches


def snakefile_of(folder):
    """The Snakefile that Snakemake finds in a folder, or `None`."""
    for name in ("Snakefile", "snakefile", "workflow/Snakefile", "workflow/snakefile"):
        if os.path.isfile(os.path.join(folder, name)):
            return name
    return None


def candidates():
    """Each workflow of the corpus, as the run corpus test names it, with its folder and the
    launches that may run it, the preferred one first."""
    catalog = os.path.join(CORPUS, "catalog")
    for name in sorted(os.listdir(catalog)):
        folder = os.path.join(catalog, name)
        if not os.path.isdir(folder) or not snakefile_of(folder):
            continue
        launches = [("plain", [])]
        for via, fields in (("ci", github_launch(folder)), ("catalog", catalog_launch(folder))):
            if fields is not None:
                launches.append((via, fields))
        yield f"catalog/{name.replace(' ', '_')}", folder, launches
    tests = os.path.join(CORPUS, "snakemake-tests/tests")
    known = tests_launches(tests)
    for name in sorted(os.listdir(tests)):
        folder = os.path.join(tests, name)
        if not os.path.isdir(folder) or not snakefile_of(folder):
            continue
        fields = known.get(name)
        launch = ("tests.py", fields) if fields else ("plain", [])
        yield f"tests/{name.replace(' ', '_')}", folder, [launch]


def read_oracle():
    """Whether real Snakemake evaluated each workflow with each launch, from the oracle file."""
    evaluated = {}
    if not os.path.exists(ORACLE):
        return evaluated
    for line in open(ORACLE, encoding="utf-8"):
        if line.startswith("#"):
            continue
        name, via, _, stage, *_ = line.rstrip("\n").split("\t")
        evaluated[(name, via)] = stage == "evaluated"
    return evaluated


def main():
    evaluated = read_oracle()
    lines = []
    for name, _, launches in candidates():
        chosen = next((launch for launch in launches if evaluated.get((name, launch[0]))), launches[0])
        via, fields = chosen
        if fields:
            lines.append((name, via, fields))
    with open(FIXTURE, "w", encoding="utf-8") as output:
        output.write("# How the run corpus test launches each workflow; written by launches.py, which says\n")
        output.write("# how it chooses. Columns, separated by tabs: the workflow, where the launch comes from\n")
        output.write("# (ci, catalog or tests.py), then fields: directory= and snakefile= relative to the\n")
        output.write("# workflow folder, configfile= relative to it, config=<key>=<value>, target=, and\n")
        output.write("# shouldfail for a test that Snakemake's own tests expect to fail. Workflows not listed\n")
        output.write("# run plainly in their folder.\n")
        for name, via, fields in lines:
            text = "\t".join(f"{key}={value}" if key != "shouldfail" else key for key, value in fields)
            output.write(f"{name}\t{via}\t{text}\n".replace("\t\n", "\n"))
    print(f"{len(lines)} launches", file=sys.stderr)


if __name__ == "__main__":
    main()
