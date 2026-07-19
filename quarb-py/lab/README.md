# quarb-lab

JupyterLab syntax highlighting for the Quarb query language — a
CodeMirror 6 `StreamLanguage` registered for the `text/x-quarb`
MIME the Quarb kernel advertises.

Ships prebuilt inside the `quarb` pip package (see
`../labextension/`, vendored so CI needs no Node); `pip install
quarb[jupyter]` places it where JupyterLab discovers it.

## Rebuild (after editing `src/index.ts`)

```sh
jlpm install        # first time; uses node_modules linker
jlpm build:prod     # -> quarb/labextension/
cp -r quarb/labextension/* ../labextension/data/share/jupyter/labextensions/quarb-lab/
```

Then rebuild the wheel (`maturin build`) — the extension rides
along as data under `share/jupyter/labextensions/`.
