# Generators that suspend at yield expressions inside other expressions.
events = []


def note(value):
    events.append(value)
    return value


def nested():
    total = note(1) + ((yield 'first') or 2)
    yield total
    items = [note('a'), (yield 'second'), *(note([3, 4]))]
    yield items
    found = {note('k'): (yield 'third'), 'n': len(items)}
    yield found
    print(note('call'), (yield 'fourth'), sep='|')
    choice = (yield 'test') if note(False) else note('else')
    yield choice
    either = note(0) or (yield 'or') or note('last')
    yield either
    both = note(1) and (yield 'and')
    yield both
    if (yield 'condition') is None:
        yield 'none'
    for item in (yield 'iterable') or [7]:
        yield item
    yield (yield 'twice')
    return (yield 'return')


print(list(nested()))
print(events)
