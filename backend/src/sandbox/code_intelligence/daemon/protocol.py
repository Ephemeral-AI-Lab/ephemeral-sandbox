"""Length-prefixed msgpack protocol for the in-sandbox CI daemon."""

from __future__ import annotations

import asyncio
import struct
from dataclasses import dataclass
from typing import Any

import msgpack

__all__ = [
    "CI_PROTOCOL_VERSION",
    "MAX_FRAME_BYTES",
    "CiRequest",
    "CiResponse",
    "FrameError",
    "SchemaError",
    "encode_frame",
    "parse_request",
    "parse_response",
    "read_frame",
]

CI_PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 64 * 1024 * 1024


class FrameError(Exception):
    """Raised when a frame cannot be encoded or decoded safely."""


class SchemaError(Exception):
    """Raised when a frame body does not match the daemon schema."""


@dataclass(frozen=True)
class CiRequest:
    """Validated daemon request body."""

    id: str
    op: str
    args: dict[str, Any]


@dataclass(frozen=True)
class CiResponse:
    """Validated daemon response body."""

    id: str
    ok: bool
    result: Any = None
    error: dict[str, Any] | None = None


def encode_frame(body: dict[str, Any]) -> bytes:
    """Encode ``body`` as ``[4-byte big-endian length][msgpack payload]``."""
    payload = msgpack.packb(body, use_bin_type=True)
    if len(payload) > MAX_FRAME_BYTES:
        raise FrameError(f"frame too large: {len(payload)}")
    return struct.pack(">I", len(payload)) + payload


async def read_frame(reader: asyncio.StreamReader) -> dict[str, Any]:
    """Read and decode one msgpack frame from ``reader``."""
    header = await reader.readexactly(4)
    (length,) = struct.unpack(">I", header)
    if length > MAX_FRAME_BYTES:
        raise FrameError(f"oversized frame header: {length}")
    body = await reader.readexactly(length)
    parsed = msgpack.unpackb(body, raw=False)
    if not isinstance(parsed, dict) or parsed.get("v") != CI_PROTOCOL_VERSION:
        raise SchemaError(f"bad schema version or shape: {parsed!r}")
    return parsed


def parse_request(body: dict[str, Any]) -> CiRequest:
    """Validate and normalize a request body."""
    request_id = body.get("id")
    op = body.get("op")
    args = body.get("args", {})
    if not isinstance(request_id, str) or not request_id:
        raise SchemaError("request id must be a non-empty string")
    if not isinstance(op, str) or not op:
        raise SchemaError("request op must be a non-empty string")
    if not isinstance(args, dict):
        raise SchemaError("request args must be a dict")
    return CiRequest(id=request_id, op=op, args=args)


def parse_response(body: dict[str, Any]) -> CiResponse:
    """Validate and normalize a response body."""
    response_id = body.get("id")
    ok = body.get("ok")
    if not isinstance(response_id, str) or not response_id:
        raise SchemaError("response id must be a non-empty string")
    if not isinstance(ok, bool):
        raise SchemaError("response ok must be a bool")
    error = body.get("error")
    if not ok and not isinstance(error, dict):
        raise SchemaError("error response must include an error dict")
    return CiResponse(
        id=response_id,
        ok=ok,
        result=body.get("result"),
        error=error if isinstance(error, dict) else None,
    )
