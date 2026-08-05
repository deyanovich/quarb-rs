import fs from "fs";

/** A lexer. */
class Lexer {
  size = 0;

  constructor(text) {
    this.text = text;
  }

  /** Scan the input. */
  lex() {
    const out = [];
    for (const c of this.text) {
      if (c === " ") {
        out.push("_");
      } else {
        out.push(c);
      }
    }
    return out;
  }

  get length() {
    return this.size;
  }
}

const helper = (a, b) => {
  let i = 0;
  while (i < a) {
    i += 1;
  }
  switch (a) {
    case 0:
      return b;
    default:
      return new Lexer("x").size;
  }
};

function main() {
  const l = new Lexer("hi");
  do {
    l.size += 1;
  } while (l.size < 2);
  return helper(l.size, 2);
}
