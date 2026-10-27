"""gdb pretty-printers for Tinox values (issue #500).

Load with `source /usr/share/tinox/tinox-gdb.py` (the `tinox dap` launcher does
this). They apply to the typedefs a `tinox build --debug` binary describes:

* `Name`        a class or immutable: shown as {field = value, ...}, `null` for 0
* `List<T>`     the runtime's {len, cap, data} array: shown as {a, b, c}
* `Map<String, V>`  the runtime's open-addressing table: shown as {["k"] = v}

`String` needs no printer: it is described as `char *`.
"""
import gdb
import gdb.printing

# Never walk more than this many elements of a container.
LIMIT = 200
# Sanity bound for a corrupt length read from uninitialised memory.
MAX_LEN = 1 << 24


def _struct_of(val):
    """(typedef name, struct type) for a typedef -> pointer -> struct value, else None."""
    t = val.type
    if t.code != gdb.TYPE_CODE_TYPEDEF or t.name is None:
        return None
    target = t.strip_typedefs()
    if target.code != gdb.TYPE_CODE_PTR:
        return None
    st = target.target().strip_typedefs()
    if st.code != gdb.TYPE_CODE_STRUCT or st.name != t.name:
        return None  # only the typedefs the Tinox compiler emits (same name as the struct)
    return t.name, st


def _nice(v):
    """Show `char *` (Tinox String) values as text only, without the address."""
    t = v.type.strip_typedefs()
    if t.code == gdb.TYPE_CODE_PTR and t.target().strip_typedefs().sizeof == 1 and int(v) != 0:
        try:
            return v.lazy_string()
        except gdb.error:
            return v
    return v


class ClassPrinter:
    def __init__(self, val, struct):
        self.val = val
        self.struct = struct

    def to_string(self):
        return "null" if int(self.val) == 0 else None

    def children(self):
        if int(self.val) == 0:
            return
        obj = self.val.dereference()
        for field in self.struct.fields():
            yield field.name, _nice(obj[field.name])


class ListPrinter:
    def __init__(self, val):
        self.val = val

    def display_hint(self):
        return "array"

    def _bounds(self):
        if int(self.val) == 0:
            return None, 0
        obj = self.val.dereference()
        n = int(obj["len"])
        return obj, n if 0 <= n <= MAX_LEN else -1

    def to_string(self):
        obj, n = self._bounds()
        if obj is None:
            return "null"
        if n < 0:
            return "<invalid List: length not initialised>"
        return None if n <= LIMIT else "len=%d (first %d)" % (n, LIMIT)

    def children(self):
        obj, n = self._bounds()
        if obj is None or n <= 0:
            return
        data = obj["data"]
        for i in range(min(n, LIMIT)):
            yield "[%d]" % i, _nice(data[i])


class MapPrinter:
    def __init__(self, val):
        self.val = val

    def display_hint(self):
        return "map"

    def _obj(self):
        return None if int(self.val) == 0 else self.val.dereference()

    def to_string(self):
        obj = self._obj()
        return "null" if obj is None else None

    def children(self):
        obj = self._obj()
        if obj is None:
            return
        cap = int(obj["cap"])
        if not 0 < cap <= MAX_LEN or int(obj["entries"]) == 0:
            return
        entries = obj["entries"]
        shown = 0
        for i in range(cap):
            entry = entries[i]
            key = int(entry["key"])
            if key in (0, 1):  # empty slot / tombstone
                continue
            yield "k%d" % i, _nice(entry["key"])
            yield "v%d" % i, _nice(entry["value"])
            shown += 1
            if shown >= LIMIT:
                return


def lookup(val):
    found = _struct_of(val)
    if found is None:
        return None
    name, struct = found
    if name.startswith("List<"):
        return ListPrinter(val)
    if name.startswith("Map<") and not name.endswith(".entry"):
        return MapPrinter(val)
    if name.endswith(".entry"):
        return None
    return ClassPrinter(val, struct)


def register(objfile=None):
    target = objfile if objfile is not None else gdb
    if lookup not in target.pretty_printers:
        target.pretty_printers.append(lookup)


register()
