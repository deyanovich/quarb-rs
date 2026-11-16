"""The JupyterLab highlighter's command words are the engine's.

The extension's keyword list was written by hand and had drifted:
aliases and retired words in it, most commands missing. It is
generated from the command inventory now (scripts/gen-lab-commands.py
writes lab/src/commands.ts), and these tests hold both the generated
source and the bundle the wheel ships to the commands the engine
lists.
"""
import re
import sys
from pathlib import Path

import quarb

WORD = re.compile(r"^[a-z][a-z0-9_]+$")


def engine_words():
    """Every command word and alias `man` lists, a retired one left out."""
    out = []
    for line in quarb.man().splitlines():
        name, _, said = line.partition(" ")
        if said.strip().startswith("retired:") or ":" in name:
            continue
        if WORD.match(name) and name not in out:
            out.append(name)
    return out


def test_the_generated_list_is_the_engines_commands():
    source = Path(__file__).resolve().parent.parent / "lab" / "src" / "commands.ts"
    listed = re.findall(r"'([a-z0-9_]+)'", source.read_text(encoding="utf-8"))
    assert listed == engine_words()
    # an alias is a word of the list, a retired command is none
    assert "wordcount" in listed and "avg" in listed and "rnd" in listed
    assert "strip" not in listed and "rec" not in listed


def test_the_shipped_bundle_holds_every_command_word():
    static = (Path(sys.prefix) / "share" / "jupyter" / "labextensions"
              / "quarb-lab" / "static")
    bundle = "".join(p.read_text(encoding="utf-8") for p in static.glob("*.js"))
    assert "x-quarb" in bundle
    missing = [w for w in engine_words()
               if f'"{w}"' not in bundle and f"'{w}'" not in bundle]
    assert not missing, f"rebuild the bundle (lab/README.md): {missing[:8]}"
