"""Exercises functions, closures, generators, classes, and exceptions."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
def greet(name, greeting="Hello", *args, punctuation="!", **kwargs):
    return f"{greeting}, {name}{punctuation} {args} {sorted(kwargs.items())}"


print(greet("Ann"), greet("Bob", "Hi", 1, 2, punctuation="?", x=1))
print(greet(*["Cy"], **{"greeting": "Yo"}))


def counter():
    count = 0

    def increment():
        nonlocal count
        count += 1
        return count

    return increment


step = counter()
step()
print(step(), step())
total = 0


def add(value):
    global total
    total += value


add(5)
add(6)
print(total)
funcs = [lambda i=i: i * 2 for i in range(3)]
late = [lambda: i for i in range(3)]
print([f() for f in funcs], [f() for f in late])


def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


print(fib(15))


def gen(n):
    for i in range(n):
        if i == 3:
            return
        yield i * i
    yield from ["never"]


print(list(gen(10)), list(gen(2)), sum(x for x in range(5)))


def decorate(function):
    def wrapper(*args):
        return "<" + function(*args) + ">"

    return wrapper


@decorate
def shout(text):
    return text.upper()


print(shout("hi"), shout.__name__)


class Animal:
    kind = "animal"

    def __init__(self, name):
        self.name = name

    def speak(self):
        return f"{self.name} makes a sound"

    def __repr__(self):
        return f"Animal({self.name!r})"

    @property
    def title(self):
        return self.name.title()

    @staticmethod
    def create(name):
        return Animal(name)

    @classmethod
    def label(cls):
        return cls.kind


class Dog(Animal):
    kind = "dog"

    def speak(self):
        return super().speak() + " (woof)"

    def __eq__(self, other):
        return isinstance(other, Dog) and self.name == other.name

    def __lt__(self, other):
        return self.name < other.name


rex = Dog("rex")
print(rex.speak(), rex.title, Animal.create("cat"), Dog.label(), rex.kind, isinstance(rex, Animal), type(rex).__name__)
print(rex == Dog("rex"), rex != Dog("max"), sorted([Dog("b"), Dog("a")], key=lambda d: d.name)[0].name, [str(Dog("z"))])


class NotFound(Exception):
    pass


def lookup(key):
    try:
        return {"a": 1}[key]
    except KeyError as error:
        raise NotFound(f"missing {key}") from error
    finally:
        print("looked up", key)


try:
    lookup("b")
except NotFound as error:
    print("caught", error, type(error).__name__, isinstance(error, Exception))
try:
    [][1]
except (IndexError, KeyError) as error:
    print("index", error)
try:
    int("x")
except ValueError as error:
    print(error)
try:
    raise ValueError("bad", 2)
except Exception as error:
    print(error.args, repr(error))
try:
    {}["k"]
except LookupError as error:
    print("lookup", repr(str(error)))
for i in range(5):
    if i == 1:
        continue
    if i == 3:
        break
    print("loop", i)
else:
    print("no break")
n = 0
while n < 3:
    n += 1
else:
    print("while done", n)
value = (lambda x, y=2: x * y)(3)
print(value, (lambda *a: a)(1, 2), sorted({"b": 1, "a": 2}, key=lambda k: k))
with open("/dev/null") as handle:
    print(handle.read() == "")
print(str(getattr(rex, "missing", "default")), hasattr(rex, "name"), callable(len), [x for x in [1, 2] if x > 1])
match {"cmd": "go", "args": [1, 2]}:
    case {"cmd": "stop"}:
        print("stop")
    case {"cmd": "go", "args": [first, *others]}:
        print("go", first, others)
from typing import Generic, Protocol, TypeVar

Item = TypeVar("Item")


class Named(Protocol):
    name: str


class Box(Generic[Item]):
    def __init__(self, item):
        self.item = item


print(Box(3).item, Named.__name__)


class Strict:
    def __getattr__(self, name):
        raise AttributeError(name)


print(getattr(Strict(), "missing", "default"))
