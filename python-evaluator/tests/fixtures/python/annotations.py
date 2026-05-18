# Annotations of functions are evaluated when the function is defined, before Python 3.14.
from typing import List, Optional


class Box:
    pass


print(int | None, isinstance(None, int | None), isinstance("a", int | float))
print(isinstance(2.5, int | float | None), (str | None).__args__)


def first(items: list[str] | None = None, *rest: int, default: Optional[List[str]] = None) -> str | None:
    return items[0] if items else None


print(first(["a", "b"]), first())

try:

    def broken(box: Box.missing) -> None:
        pass

except AttributeError as error:
    print("AttributeError:", error)
