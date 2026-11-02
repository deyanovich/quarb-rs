"""Helpers for the python: environment tests (spec: Environment
Mounts): plain functions, mounted as `py=python:helpers.py`."""
import re


def shout(s):
    """Upper-case the text."""
    return s.upper()


def round_to(x, n=0):
    return round(x, n)


def sub(pattern, repl, s):
    return re.sub(pattern, repl, s)


def tag(data=None, k="t"):
    return {k: data}


def total(xs):
    return sum(xs)


def spread(xs):
    """The items doubled, one value each."""
    return [x * 2 for x in xs]


def fail(x):
    raise ValueError("no")


def read_file(path):
    with open(path) as f:
        return f.read()


def kind(v):
    return type(v).__name__
