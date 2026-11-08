#!/usr/bin/env python3
"""Proto3 messages from a web `*_pb2.py`, without the protobuf package.

A web feed that receives protobuf parses it with classes protoc generated
into a `*_pb2.py`, which needs the `protobuf` package the corpus tools do
not install. The module's schema is a serialized `FileDescriptorProto`
passed to `AddSerializedFile`; this file reads it by AST (the web module is
never executed) and builds plain classes that decode as protobuf's Python
classes do, for what a feed parser reads:

- `Message()`, `ParseFromString(data)`, attribute access with proto3
  defaults (a message field never set reads as an empty message), repeated
  fields as lists, `HasField(name)` for message fields;
- the file's top-level enum values as module attributes (`pb.NSE_FO_ALL`);
- `DecodeError` for a truncated or malformed frame. A field whose wire type
  does not match the schema is skipped as an unknown field, as protobuf's
  own parser keeps it aside rather than failing.

Decoding, in a glue (`web_adapters/<broker>.py`): hand the stand-in to the
web parser in place of its `*_pb2` import, under the name it imports:

    pb = protolite.module(Path(web_root) / "broker/<broker>/streaming/<name>_pb2.py")
    stubs = {"pb": pb, "DecodeError": pb.DecodeError}

Encoding a corpus frame: `encode` turns a message spec (field names and
values, enum values by name, nested messages as objects, repeated fields as
lists) into the `.hex` text the harness reads, one line per field with its
name and value as a comment, so a case's frame says field by field what it
carries. Only the fields a spec names are written, as a proto3 sender
leaves defaults out:

    python3 scripts/feed_corpus/protolite.py --web ../openalgo \\
        --pb2 broker/<broker>/streaming/<name>_pb2.py \\
        --type <Message> --comment "what the frame is" spec.json > frame.hex

Standard library only. Used by the HDFC Sky and HDFC Securities glues.
"""

from __future__ import annotations

import argparse
import ast
import json
import struct
import sys
import types
from pathlib import Path
from typing import Any

# FieldDescriptorProto.Type
DOUBLE, FLOAT, INT64, UINT64, INT32, FIXED64, FIXED32, BOOL, STRING = 1, 2, 3, 4, 5, 6, 7, 8, 9
MESSAGE, BYTES, UINT32, ENUM, SFIXED32, SFIXED64, SINT32, SINT64 = 11, 12, 13, 14, 15, 16, 17, 18
REPEATED = 3

VARINT_TYPES = {INT64, UINT64, INT32, BOOL, UINT32, ENUM, SINT32, SINT64}
WIRE = {DOUBLE: 1, FIXED64: 1, SFIXED64: 1, FLOAT: 5, FIXED32: 5, SFIXED32: 5,
        STRING: 2, BYTES: 2, MESSAGE: 2}


class DecodeError(Exception):
    """A frame that is not a well-formed protobuf message."""


# -- the wire format --------------------------------------------------------

def _varint(buf: bytes, i: int) -> tuple[int, int]:
    shift = value = 0
    while True:
        if i >= len(buf):
            raise DecodeError("truncated varint")
        b = buf[i]
        i += 1
        value |= (b & 0x7F) << shift
        if not b & 0x80:
            return value, i
        shift += 7
        if shift >= 70:
            raise DecodeError("varint too long")


def _fields(buf: bytes):
    """(field number, wire type, raw value) for each field of a message."""
    i = 0
    while i < len(buf):
        key, i = _varint(buf, i)
        number, wt = key >> 3, key & 7
        if number == 0:
            raise DecodeError("field number 0")
        if wt == 0:
            v, i = _varint(buf, i)
        elif wt == 1:
            if i + 8 > len(buf):
                raise DecodeError("truncated fixed64")
            v, i = buf[i:i + 8], i + 8
        elif wt == 2:
            n, i = _varint(buf, i)
            if i + n > len(buf):
                raise DecodeError("truncated length-delimited field")
            v, i = buf[i:i + n], i + n
        elif wt == 5:
            if i + 4 > len(buf):
                raise DecodeError("truncated fixed32")
            v, i = buf[i:i + 4], i + 4
        else:
            raise DecodeError(f"wire type {wt}")
        yield number, wt, v


def _signed(v: int, bits: int) -> int:
    v &= (1 << 64) - 1
    if v >= 1 << 63:
        v -= 1 << 64
    if bits == 32:
        v = ((v + (1 << 31)) % (1 << 32)) - (1 << 31)
    return v


def _scalar(ftype: int, wt: int, raw):
    if ftype in (INT64, ENUM):
        return _signed(raw, 64) if ftype == INT64 else _signed(raw, 32)
    if ftype == INT32:
        return _signed(raw, 32)
    if ftype in (UINT64, UINT32):
        return raw
    if ftype == BOOL:
        return raw != 0
    if ftype in (SINT32, SINT64):
        return (raw >> 1) ^ -(raw & 1)
    if ftype == DOUBLE:
        return struct.unpack("<d", raw)[0]
    if ftype == FLOAT:
        return struct.unpack("<f", raw)[0]
    if ftype == FIXED64:
        return struct.unpack("<Q", raw)[0]
    if ftype == SFIXED64:
        return struct.unpack("<q", raw)[0]
    if ftype == FIXED32:
        return struct.unpack("<I", raw)[0]
    if ftype == SFIXED32:
        return struct.unpack("<i", raw)[0]
    if ftype == STRING:
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError as e:
            raise DecodeError("string field is not UTF-8") from e
    if ftype == BYTES:
        return bytes(raw)
    raise DecodeError(f"field type {ftype}")


def _wire_of(ftype: int) -> int:
    return 0 if ftype in VARINT_TYPES else WIRE[ftype]


# -- the schema -------------------------------------------------------------

class Field:
    def __init__(self, name: str, number: int, label: int, ftype: int, type_name: str):
        self.name, self.number, self.label, self.type, self.type_name = (
            name, number, label, ftype, type_name.lstrip(".").split(".")[-1])

    @property
    def repeated(self) -> bool:
        return self.label == REPEATED

    def default(self):
        if self.repeated:
            return []
        if self.type in (STRING,):
            return ""
        if self.type == BYTES:
            return b""
        if self.type == BOOL:
            return False
        if self.type in (DOUBLE, FLOAT):
            return 0.0
        return 0


def _strings(buf: bytes, number: int) -> str:
    for n, wt, v in _fields(buf):
        if n == number and wt == 2:
            return v.decode("utf-8")
    return ""


def _schema(descriptor: bytes) -> tuple[dict[str, list[Field]], dict[str, dict[str, int]]]:
    """Messages (with nested ones, by simple name) and top-level enums of a
    serialized FileDescriptorProto."""
    messages: dict[str, list[Field]] = {}
    enums: dict[str, dict[str, int]] = {}

    def enum(buf: bytes) -> tuple[str, dict[str, int]]:
        values = {}
        for n, wt, v in _fields(buf):
            if n == 2 and wt == 2:
                num = 0
                for n2, wt2, v2 in _fields(v):
                    if n2 == 2 and wt2 == 0:
                        num = _signed(v2, 32)
                values[_strings(v, 1)] = num
        return _strings(buf, 1), values

    def message(buf: bytes) -> None:
        fields = []
        for n, wt, v in _fields(buf):
            if n == 2 and wt == 2:
                number = label = ftype = 0
                for n2, wt2, v2 in _fields(v):
                    if wt2 == 0 and n2 == 3:
                        number = v2
                    elif wt2 == 0 and n2 == 4:
                        label = v2
                    elif wt2 == 0 and n2 == 5:
                        ftype = v2
                fields.append(Field(_strings(v, 1), number, label, ftype, _strings(v, 6)))
            elif n == 3 and wt == 2:
                message(v)
        messages[_strings(buf, 1)] = fields

    for n, wt, v in _fields(descriptor):
        if n == 4 and wt == 2:
            message(v)
        elif n == 5 and wt == 2:
            name, values = enum(v)
            enums[name] = values
    return messages, enums


def descriptor_of(pb2: Path) -> bytes:
    """The serialized file descriptor a `*_pb2.py` registers, read by AST."""
    tree = ast.parse(pb2.read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr == "AddSerializedFile" and node.args
            and isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, bytes)
        ):
            return node.args[0].value
    raise SystemExit(f"{pb2}: no AddSerializedFile(b'...') call")


# -- decoding ---------------------------------------------------------------

class _Message:
    _fields: dict[str, Field] = {}
    _numbers: dict[int, Field] = {}
    _module: types.ModuleType

    def __init__(self):
        self._set: dict[str, Any] = {}

    def __getattr__(self, name: str):
        fields = type(self)._fields
        if name not in fields:
            raise AttributeError(name)
        f = fields[name]
        if name in self._set:
            return self._set[name]
        if f.repeated:
            self._set[name] = []
            return self._set[name]
        if f.type == MESSAGE:
            return getattr(self._module, f.type_name)()
        return f.default()

    def HasField(self, name: str) -> bool:  # noqa: N802 (protobuf's name)
        f = type(self)._fields.get(name)
        if f is None or f.repeated or f.type != MESSAGE:
            raise ValueError(f"{type(self).__name__} has no message field {name!r}")
        return name in self._set

    def ParseFromString(self, data: bytes) -> int:  # noqa: N802 (protobuf's name)
        self._set = {}
        self._merge(bytes(data))
        return len(data)

    def _merge(self, data: bytes) -> None:
        for number, wt, raw in _fields(data):
            f = type(self)._numbers.get(number)
            if f is None:
                continue
            if f.type == MESSAGE:
                if wt != 2:
                    continue
                sub = getattr(self._module, f.type_name)()
                if not f.repeated and f.name in self._set:
                    sub = self._set[f.name]
                sub._merge(raw)
                if f.repeated:
                    self._set.setdefault(f.name, []).append(sub)
                else:
                    self._set[f.name] = sub
            elif f.repeated and wt == 2 and f.type not in (STRING, BYTES):
                # A packed repeated scalar.
                out = self._set.setdefault(f.name, [])
                i = 0
                while i < len(raw):
                    if f.type in VARINT_TYPES:
                        v, i = _varint(raw, i)
                        out.append(_scalar(f.type, 0, v))
                    else:
                        size = 8 if WIRE[f.type] == 1 else 4
                        out.append(_scalar(f.type, WIRE[f.type], raw[i:i + size]))
                        i += size
            elif wt == _wire_of(f.type):
                v = _scalar(f.type, wt, raw)
                if f.repeated:
                    self._set.setdefault(f.name, []).append(v)
                else:
                    self._set[f.name] = v


def module(pb2: Path) -> types.ModuleType:
    """The `*_pb2` stand-in: a class per message, the top-level enum values,
    and `DecodeError`."""
    messages, enums = _schema(descriptor_of(pb2))
    m = types.ModuleType(pb2.stem)
    m.DecodeError = DecodeError
    for values in enums.values():
        for name, number in values.items():
            setattr(m, name, number)
    m._enums = enums
    m._messages = messages
    for name, fields in messages.items():
        cls = type(name, (_Message,), {
            "_fields": {f.name: f for f in fields},
            "_numbers": {f.number: f for f in fields},
            "_module": m,
        })
        setattr(m, name, cls)
    return m


# -- encoding a corpus frame --------------------------------------------------

def _uvarint(v: int) -> bytes:
    v &= (1 << 64) - 1
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _key(f: Field, wt: int) -> bytes:
    return _uvarint(f.number << 3 | wt)


def _encode_scalar(m: types.ModuleType, f: Field, v) -> bytes:
    if f.type == ENUM:
        if isinstance(v, str):
            v = getattr(m, v)
        return _uvarint(v)
    if f.type in (INT64, INT32, UINT64, UINT32):
        return _uvarint(int(v))
    if f.type == BOOL:
        return _uvarint(1 if v else 0)
    if f.type in (SINT32, SINT64):
        v = int(v)
        return _uvarint((v << 1) ^ (v >> 63))
    if f.type == DOUBLE:
        return struct.pack("<d", float(v))
    if f.type == FLOAT:
        return struct.pack("<f", float(v))
    if f.type == STRING:
        b = v.encode("utf-8")
        return _uvarint(len(b)) + b
    raise SystemExit(f"field {f.name}: type {f.type} is not written by this encoder")


def encode(m: types.ModuleType, type_name: str, spec: dict, depth: int = 0) -> list[tuple[bytes, str]]:
    """`spec` as protobuf `type_name`: (bytes, comment) lines, a nested
    message's lines after the line that opens it."""
    fields = {f.name: f for f in m._messages[type_name]}
    lines: list[tuple[bytes, str]] = []
    pad = "  " * depth
    for name, value in spec.items():
        f = fields.get(name)
        if f is None:
            raise SystemExit(f"{type_name} has no field {name!r}")
        for v in (value if f.repeated else [value]):
            if f.type == MESSAGE:
                inner = encode(m, f.type_name, v, depth + 1)
                size = sum(len(b) for b, _ in inner)
                lines.append((_key(f, 2) + _uvarint(size), f"{pad}{name} ({f.type_name}, {size} bytes)"))
                lines.extend(inner)
            else:
                body = _encode_scalar(m, f, v)
                lines.append((_key(f, _wire_of(f.type)) + body, f"{pad}{name} = {v}"))
    return lines


def hex_text(lines: list[tuple[bytes, str]], header: list[str]) -> str:
    width = max(len(b.hex()) for b, _ in lines)
    out = [f"# {h}" for h in header]
    out += [f"{b.hex():<{width}}  # {c}" for b, c in lines]
    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--web", required=True, type=Path, help="the web tree (read only)")
    ap.add_argument("--pb2", required=True, help="the *_pb2.py, relative to the web tree")
    ap.add_argument("--type", required=True, help="the message to write")
    ap.add_argument("--comment", action="append", default=[], help="a header comment line")
    ap.add_argument("spec", type=Path, help="JSON: field names and values")
    args = ap.parse_args()
    m = module(args.web / args.pb2)
    lines = encode(m, args.type, json.loads(args.spec.read_text()))
    sys.stdout.write(hex_text(lines, args.comment))
    return 0


if __name__ == "__main__":
    sys.exit(main())
