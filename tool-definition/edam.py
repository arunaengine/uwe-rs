"""Rebuilds edam.tsv, the EDAM class hierarchy of tool-definition, from an EDAM release."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import sys
import urllib.request
import xml.etree.ElementTree as ElementTree

RELEASES = "https://github.com/edamontology/edamontology/releases/download"
BASE = "http://edamontology.org/"
BRANCHES = ("data_", "format_", "operation_", "topic_")
OWL = "{http://www.w3.org/2002/07/owl#}"
RDF = "{http://www.w3.org/1999/02/22-rdf-syntax-ns#}"
RDFS = "{http://www.w3.org/2000/01/rdf-schema#}"


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit("usage: edam.py <release tag> <output tsv> [local EDAM.owl of that release]")
    tag, output = sys.argv[1], sys.argv[2]
    address = f"{RELEASES}/{tag}/EDAM.owl"
    if len(sys.argv) == 4:
        with open(sys.argv[3], "rb") as file:
            data = file.read()
    else:
        with urllib.request.urlopen(address) as response:
            data = response.read()
    classes = read_classes(ElementTree.fromstring(data))
    with open(output, "w", encoding="utf-8", newline="\n") as file:
        file.write(f"# EDAM {tag} from {address}\n")
        file.write("# CC BY-SA 4.0, see LICENSE-EDAM. Columns: id, label, parents.\n")
        for term in sorted(classes):
            label, parents = classes[term]
            known = sorted(parent for parent in parents if parent in classes)
            file.write(f"{term}\t{label}\t{' '.join(known)}\n")


def read_classes(root):
    """The classes of the four branches that are not deprecated: id to label and parents."""
    classes = {}
    for element in root.iter(f"{OWL}Class"):
        about = element.get(f"{RDF}about", "")
        term = about.removeprefix(BASE)
        if term == about or not term.startswith(BRANCHES):
            continue
        if (element.findtext(f"{OWL}deprecated") or "").strip() == "true":
            continue
        label = " ".join((element.findtext(f"{RDFS}label") or term).split())
        parents = [
            parent.get(f"{RDF}resource", "").removeprefix(BASE)
            for parent in element.findall(f"{RDFS}subClassOf")
            if parent.get(f"{RDF}resource", "").startswith(BASE)
        ]
        classes[term] = (label, parents)
    return classes


if __name__ == "__main__":
    main()
