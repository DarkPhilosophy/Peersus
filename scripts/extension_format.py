"""Data-only WebAssembly custom sections shared by packaging and submission checks."""


def uleb(value: int) -> bytes:
    result = bytearray()
    while value >= 128:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def _length(data: bytes, at: int, end: int) -> tuple[int, int]:
    value = 0
    for shift in range(0, 35, 7):
        if at >= end:
            break
        byte = data[at]
        at += 1
        if shift == 28 and byte > 15:
            break
        value |= (byte & 127) << shift
        if byte < 128:
            return value, at
    raise ValueError("invalid WebAssembly section length")


def wasm_custom_sections(data: bytes, wanted: str) -> list[bytes] | None:
    if not data.startswith(b"\0asm\x01\0\0\0"):
        return None
    at, found = 8, []
    while at < len(data):
        section = data[at]
        size, body = _length(data, at + 1, len(data))
        end = body + size
        if end > len(data):
            raise ValueError("truncated WebAssembly section")
        if section == 0:
            size, name = _length(data, body, end)
            if name + size > end:
                raise ValueError("truncated WebAssembly custom section name")
            if data[name:name + size] == wanted.encode("utf-8"):
                found.append(data[name + size:end])
        at = end
    return found
