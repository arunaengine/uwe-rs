# Enumerations: members, lookups by value and name, iteration, and auto numbers.
import enum
from enum import Enum, IntEnum, StrEnum, auto, unique


class Genotype(enum.Enum):
    HOMOZYG_NOSV = 'hmz'
    HOMOZYG_SV = 'hmz-sv'

    @classmethod
    def names(cls):
        return [member.name for member in cls]

    def label(self):
        return self.value.upper()


@unique
class Color(Enum):
    RED = auto()
    GREEN = auto()


class Level(IntEnum):
    LOW = 1
    HIGH = 2


class Kind(StrEnum):
    FILE = "file"


print(Genotype.HOMOZYG_SV, repr(Genotype.HOMOZYG_NOSV), Genotype.HOMOZYG_SV.value)
print(Genotype("hmz") is Genotype.HOMOZYG_NOSV, Genotype["HOMOZYG_SV"].name)
print(list(Genotype), len(Genotype), Genotype.names(), Genotype.HOMOZYG_SV.label())
print(Genotype.HOMOZYG_NOSV in Genotype, isinstance(Genotype.HOMOZYG_SV, Genotype))
print(Color.RED.value, Color.GREEN.value, Color(2).name, list(Color.__members__))
print(Level.HIGH == 2, Level(1).name, str(Kind.FILE), Kind.FILE == "file")
print({Genotype.HOMOZYG_SV: 1}[Genotype("hmz-sv")])
try:
    Genotype("other")
except ValueError as error:
    print(error)
try:
    Genotype["OTHER"]
except KeyError as error:
    print("KeyError", error)
