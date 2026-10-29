"""Minimal Ascon-Hash256 implementation matching the Native ELF loader."""

from __future__ import annotations

_MASK64 = (1 << 64) - 1
_ROUNDS = (0xF0, 0xE1, 0xD2, 0xC3, 0xB4, 0xA5, 0x96, 0x87, 0x78, 0x69, 0x5A, 0x4B)


def _ror(value: int, count: int) -> int:
    return ((value >> count) | (value << ((64 - count) & 63))) & _MASK64


def _round(state: list[int], constant: int) -> None:
    state[2] ^= constant
    state[0] ^= state[4]
    state[4] ^= state[3]
    state[2] ^= state[1]
    t0 = state[0] ^ ((~state[1]) & state[2])
    t1 = state[1] ^ ((~state[2]) & state[3])
    t2 = state[2] ^ ((~state[3]) & state[4])
    t3 = state[3] ^ ((~state[4]) & state[0])
    t4 = state[4] ^ ((~state[0]) & state[1])
    t1 ^= t0
    t0 ^= t4
    t3 ^= t2
    t2 = (~t2) & _MASK64
    state[0] = (t0 ^ _ror(t0, 19) ^ _ror(t0, 28)) & _MASK64
    state[1] = (t1 ^ _ror(t1, 61) ^ _ror(t1, 39)) & _MASK64
    state[2] = (t2 ^ _ror(t2, 1) ^ _ror(t2, 6)) & _MASK64
    state[3] = (t3 ^ _ror(t3, 10) ^ _ror(t3, 17)) & _MASK64
    state[4] = (t4 ^ _ror(t4, 7) ^ _ror(t4, 41)) & _MASK64


def _permute(state: list[int]) -> None:
    for constant in _ROUNDS:
        _round(state, constant)


def hash256(data: bytes) -> bytes:
    """Return the 32-byte Ascon-Hash256 digest used by the bootstrap ELF."""
    state = [0x0000080100CC0002, 0, 0, 0, 0]
    _permute(state)
    offset = 0
    length = len(data)
    while length >= 8:
        state[0] ^= int.from_bytes(data[offset : offset + 8], "little")
        _permute(state)
        offset += 8
        length -= 8
    state[0] ^= int.from_bytes(data[offset:], "little")
    state[0] ^= 1 << (8 * length)
    _permute(state)
    output = bytearray()
    for _ in range(4):
        output.extend(state[0].to_bytes(8, "little"))
        _permute(state)
    return bytes(output)


def hexdigest(data: bytes) -> str:
    return hash256(data).hex()
