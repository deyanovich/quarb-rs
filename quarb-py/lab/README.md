# quarb-lab

JupyterLab syntax highlighting for the Quarb query language — a
CodeMirror 6 `StreamLanguage` registered for the `text/x-quarb`
MIME the Quarb kernel advertises.

Ships prebuilt inside the `quarb` pip package (see
`../labextension/`, vendored so CI needs no Node); `pip install
quarb[jupyter]` places it where JupyterLab discovers it.

## The command words

The words highlighted as keywords are the engine's: every command
of the built-in library and its aliases. `src/commands.ts` is
generated from the command inventory and never edited by hand:

```sh
../../scripts/gen-lab-commands.py    # from the engine root's scripts/
```

Run it whenever the inventory is vendored again, then rebuild. The
Python tests (`../tests/test_lab.py`) hold both the generated list
and the bundle the wheel ships to the commands `quarb.man()` lists,
so a stale list or a bundle that was not rebuilt fails the gate.

## Rebuild (after editing `src/index.ts` or `src/commands.ts`)

`jlpm` and the builder come with JupyterLab (`pip install
jupyterlab` in an environment of its own).

```sh
jlpm install        # first time; uses node_modules linker
jlpm build:prod     # -> quarb/labextension/
rm -r ../labextension/data/share/jupyter/labextensions/quarb-lab/static
cp -r quarb/labextension/* ../labextension/data/share/jupyter/labextensions/quarb-lab/
```

The old `static/` goes first: the bundle's file names carry a hash,
and a stale one would ship beside the new.

Then rebuild the wheel (`maturin build`) — the extension rides
along as data under `share/jupyter/labextensions/`.
