#!/usr/bin/env -S uv run -q --with ruamel.yaml python3
# Finds the example job files of the real-world CWL documents of the corpus from
# `just cwl-corpus` (CWL_CORPUS), and prints one line per pair: the document and the job file,
# relative to the corpus. A job file pairs with a document of the same repository when the
# document has every input the job sets, the job sets every required input without default,
# and the name of the job file holds the name of the document, or the document is the only
# such one next to it. job_corpus.txt starts from its output.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import os
import sys

from ruamel.yaml import YAML

yaml = YAML(typ='safe', pure=True)
corpus = os.environ.get('CWL_CORPUS') or os.path.expanduser('~/.cache/iunctus/corpus/cwl')
root = os.path.join(corpus, 'realworld')
docs = {}   # path -> (class, inputs{name: (required)})
jobs = []
def load(path):
    try:
        with open(path) as f: return yaml.load(f)
    except Exception: return None
def inputs_of(doc):
    ins = doc.get('inputs', {})
    out = {}
    if isinstance(ins, dict):
        for k, v in ins.items():
            t = v if not isinstance(v, dict) else v.get('type')
            opt = (isinstance(t, str) and t.endswith('?')) or (isinstance(t, list) and 'null' in t) or (isinstance(v, dict) and 'default' in v)
            out[k] = not opt
    elif isinstance(ins, list):
        for v in ins:
            if isinstance(v, dict) and 'id' in v:
                t = v.get('type'); k = v['id'].split('#')[-1].split('/')[-1]
                opt = (isinstance(t, str) and t.endswith('?')) or (isinstance(t, list) and 'null' in t) or 'default' in v
                out[k] = not opt
    return out
for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = [d for d in dirnames if d != '.git']
    for name in filenames:
        if not name.lower().endswith(('.cwl', '.yml', '.yaml', '.json')): continue
        path = os.path.join(dirpath, name)
        doc = load(path)
        if not isinstance(doc, dict): continue
        if doc.get('class') in ('Workflow', 'CommandLineTool', 'ExpressionTool') and 'cwlVersion' in doc:
            docs[path] = (doc.get('class'), inputs_of(doc))
        elif 'cwlVersion' not in doc and '$graph' not in doc and 'class' not in doc and doc:
            jobs.append((path, doc))
def repo(path):
    rel = os.path.relpath(path, root).split('/')
    return '/'.join(rel[:2])
pairs = []
for jpath, job in jobs:
    keys = set(job.keys())
    if not keys or any(k.startswith(('$', 's:', 'cwl')) for k in keys): continue
    best = []
    for dpath, (cls, ins) in docs.items():
        if repo(dpath) != repo(jpath): continue
        if not keys <= set(ins): continue
        missing = [k for k, req in ins.items() if req and k not in keys]
        if missing: continue
        best.append(dpath)
    jstem = os.path.basename(jpath).rsplit('.', 1)[0].lower()
    named = [d for d in best if os.path.basename(d).rsplit('.', 1)[0].lower() in jstem]
    beside = [d for d in best if os.path.dirname(d) == os.path.dirname(jpath)]
    if named:
        named.sort(key=lambda d: (-len(os.path.basename(d)), os.path.dirname(d) != os.path.dirname(jpath)))
        pairs.append((jpath, named[0], len(named)))
    elif len(beside) == 1:
        pairs.append((jpath, beside[0], 1))
for j, d, n in sorted(pairs, key=lambda pair: (pair[1], pair[0])):
    print(os.path.relpath(d, corpus), os.path.relpath(j, corpus))
print(len(pairs), 'pairs of', len(jobs), 'files that may be jobs', file=sys.stderr)
