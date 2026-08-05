import os
from sys import path


class Lexer:
    """A lexer."""

    def __init__(self, text):
        self.text = text

    def lex(self):
        """Scan the input."""
        out = []
        for c in self.text:
            if c.isalnum():
                out.append(c)
            elif c == " ":
                out.append("_")
            else:
                out.append(".")
        return out


def helper(a, b):
    f = lambda x: x + 1
    while a < b:
        a = f(a)
    match a:
        case 0:
            return 0
        case _:
            return a


def run(n):
    return helper(n, 2)
