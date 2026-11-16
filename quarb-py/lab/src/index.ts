// JupyterLab syntax highlighting for the Quarb query language.
//
// Registers a CodeMirror 6 StreamLanguage under the name/MIME the
// Quarb kernel advertises (language_info.codemirror_mode = "quarb",
// mimetype = "text/x-quarb"), so cells in a Quarb notebook — and
// any editor opened as text/x-quarb — get highlighted.
//
// A StreamLanguage (a token(stream,state) tokenizer) is the
// pragmatic choice for a query language: no Lezer grammar, just
// the sigils, axes, functions, strings, numbers, and units that
// make Quarb read as Quarb.

import {
  JupyterFrontEnd,
  JupyterFrontEndPlugin
} from '@jupyterlab/application';
import { IEditorLanguageRegistry } from '@jupyterlab/codemirror';
import {
  StreamLanguage,
  StringStream,
  LanguageSupport
} from '@codemirror/language';
import { COMMANDS } from './commands';

// The words highlighted as keywords: every command of the built-in
// library and its aliases, generated from the engine's command
// inventory (commands.ts; an alias and a command are declared there
// once), and the two words that open a definition.
const KEYWORDS = new Set<string>([...COMMANDS, 'def', 'macro']);

interface State {
  // inside a string; the closing quote, or 0
  quote: number;
}

// Longest-match operator table. Order matters: try longer sigils
// before their prefixes (`:::` before `::`, `<=>?` before `<=>`).
const OPERATORS = [
  ':::', '::', // the colon ladder: trait keys, properties
  '<=>*?', '<=>*', '<=>?', '<=>', // joins: every witness, outer
  '-->', '<--', '=>', '->', '<-', '--', // cross-references, the value match, links
  '...?', '...', // spread
  '@|', '$|', '|', // pipes
  '%.', '@.', '$.', '$$', '$-', // register / record / context refs
  '??', '&&', '||', // null default, logic
  '!==', '!=', '==', '!', // the pattern comparison, inequality, negation
  '?=', '>=', '<=', '=', '<', '>', // comparisons
  '//', '/', '\\\\', '\\', // axes
  '+', '{', '}', '?' // quantifiers / reach
];

function tokenizer(stream: StringStream, state: State): string | null {
  // Continue a string.
  if (state.quote) {
    while (!stream.eol()) {
      const ch = stream.next();
      if (ch === '\\') {
        stream.next();
      } else if (ch && ch.charCodeAt(0) === state.quote) {
        state.quote = 0;
        return 'string';
      }
    }
    return 'string';
  }

  if (stream.eatSpace()) {
    return null;
  }

  const ch = stream.peek();
  if (ch == null) {
    return null;
  }

  // Strings.
  if (ch === '"' || ch === "'") {
    state.quote = ch.charCodeAt(0);
    stream.next();
    return tokenizer(stream, state);
  }

  // A register/context reference like $*1, $.name, $$ — the sigil
  // is an operator, a trailing name is a variable.
  if (ch === '$') {
    stream.next();
    stream.eat('*') || stream.eat('.') || stream.eat('$') || stream.eat('-');
    if (stream.match(/^[A-Za-z0-9_]+/)) {
      return 'variableName';
    }
    return 'operator';
  }

  // A register push `.name(` or recall `.name` — the leading dot is
  // structure, the name a variable.
  if (ch === '.' && stream.match(/^\.[A-Za-z_][A-Za-z0-9_-]*/)) {
    return 'variableName';
  }

  // Numbers, incl. unit/span suffixes (5km, 90min, 1.5h, 100kB).
  if (/[0-9]/.test(ch)) {
    stream.match(/^[0-9]+(\.[0-9]+)?/);
    if (stream.match(/^[A-Za-z][A-Za-z0-9^*/%-]*/)) {
      return 'unit'; // a quantity/duration literal
    }
    return 'number';
  }

  // Identifiers: keywords vs plain names (property/edge names).
  if (/[A-Za-z_]/.test(ch)) {
    stream.match(/^[A-Za-z_][A-Za-z0-9_-]*/);
    const word = stream.current();
    if (KEYWORDS.has(word)) {
      return 'keyword';
    }
    return 'propertyName';
  }

  // Operators / sigils (longest match).
  for (const op of OPERATORS) {
    if (stream.match(op)) {
      return 'operator';
    }
  }

  stream.next();
  return null;
}

const quarbLanguage = StreamLanguage.define<State>({
  name: 'quarb',
  startState: () => ({ quote: 0 }),
  token: tokenizer,
  languageData: {
    name: 'quarb',
    // `//` is not a comment in Quarb — it is the descendant axis —
    // so no commentTokens.
    closeBrackets: { brackets: ['(', '[', '{', '"', "'"] }
  }
});

const plugin: JupyterFrontEndPlugin<void> = {
  id: 'quarb-lab:highlighting',
  description: 'Syntax highlighting for the Quarb query language.',
  autoStart: true,
  requires: [IEditorLanguageRegistry],
  activate: (_app: JupyterFrontEnd, languages: IEditorLanguageRegistry) => {
    languages.addLanguage({
      name: 'quarb',
      // The kernel advertises text/x-quarb; also match the bare name.
      mime: ['text/x-quarb', 'text/quarb'],
      support: new LanguageSupport(quarbLanguage),
      extensions: ['quarb']
    });
  }
};

export default plugin;
