#!/usr/bin/env python3
"""RPM wire protocol for slight_replica (Jorge sd:/slight/ paths)."""

from __future__ import annotations

import json
from typing import Any

TCP_PORT = 7878
FRAME_OPEN = "<TCP_MESSAGE>"
FRAME_CLOSE = "</TCP_MESSAGE>"


def wrap_frame(inner: dict | str) -> str:
    if isinstance(inner, dict):
        inner = json.dumps(inner, separators=(",", ":"))
    return f"{FRAME_OPEN}{inner}{FRAME_CLOSE}"


def build_envelope(header: str, body_obj: dict) -> str:
    inner = {"header": header, "body": json.dumps(body_obj, separators=(",", ":"))}
    return wrap_frame(inner)


def build_notify(effect_id: int, name: str, effect_data: dict) -> str:
    value_in_json = json.dumps(effect_data, separators=(",", ":"))
    body = {"Notify": {"id": effect_id, "name": name, "value_in_json": value_in_json}}
    return build_envelope("Notify", body)


def build_remove(effect_id: int) -> str:
    return build_envelope("Remove", {"Remove": {"id": effect_id}})


def build_remove_all() -> str:
    return build_envelope("RemoveAll", {})


def build_give_client_id(client_id: int) -> str:
    return build_envelope("GiveClientId", {"GiveClientId": {"client_id": client_id}})


def parse_frames(buf: str) -> tuple[list[dict], str]:
    out: list[dict] = []
    while True:
        start = buf.find(FRAME_OPEN)
        end = buf.find(FRAME_CLOSE)
        if start == -1 or end == -1 or end < start:
            break
        payload = buf[start + len(FRAME_OPEN) : end]
        buf = buf[end + len(FRAME_CLOSE) :]
        out.append(json.loads(payload))
    return out, buf


def encode_update_message(effect_id: int, new_value: dict | str) -> str:
    if isinstance(new_value, dict):
        new_value = json.dumps(new_value, separators=(",", ":"))
    return json.dumps({"id": effect_id, "newValue": new_value}, separators=(",", ":"))


def decode_update_message(raw: str) -> tuple[int, dict]:
    msg = json.loads(raw)
    effect_id = int(msg["id"])
    nv = msg["newValue"]
    if isinstance(nv, str):
        nv = json.loads(nv)
    return effect_id, nv


def flatten_new_value(prefix: str, val: Any, out: list[dict]) -> None:
    if isinstance(val, dict):
        for k, v in val.items():
            key = f"{prefix}.{k}" if prefix else k
            flatten_new_value(key, v, out)
    else:
        out.append({"path": prefix, "value": val})


def transaction_filename(object_id: int, client_id: int, transaction_id: int) -> str:
    return f"object-{object_id}-client-{client_id}-transaction-{transaction_id}"
