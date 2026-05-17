from __future__ import annotations

import ast
import json
from typing import Any


def loads(text: str) -> Any:
    stripped = text.lstrip()
    if stripped.startswith("{") or stripped.startswith("["):
        return json.loads(text)
    lines = text.splitlines()
    value, index = _parse_block(lines, 0, 0)
    while index < len(lines) and not _content(lines[index]):
        index += 1
    if index != len(lines):
        raise ValueError(f"unexpected content at line {index + 1}: {lines[index]}")
    return value


def dumps(value: Any, indent: int = 0) -> str:
    lines = _dump(value, indent)
    return "\n".join(lines) + "\n"


def _parse_block(lines: list[str], index: int, indent: int) -> tuple[Any, int]:
    index = _skip_blank(lines, index)
    if index >= len(lines):
        return None, index
    current_indent = _indent(lines[index])
    if current_indent < indent:
        return None, index
    if _stripped(lines[index]).startswith("- "):
        return _parse_list(lines, index, current_indent)
    return _parse_dict(lines, index, current_indent)


def _parse_dict(lines: list[str], index: int, indent: int) -> tuple[dict[str, Any], int]:
    obj: dict[str, Any] = {}
    while index < len(lines):
        if not _content(lines[index]):
            index += 1
            continue
        line_indent = _indent(lines[index])
        if line_indent < indent:
            break
        if line_indent > indent:
            raise ValueError(f"unexpected indentation at line {index + 1}: {lines[index]}")
        stripped = _stripped(lines[index])
        if stripped.startswith("- "):
            break
        key, raw = _split_key_value(stripped, index)
        if raw == "|":
            value, index = _parse_block_scalar(lines, index + 1, indent + 2)
        elif raw == "":
            value, index = _parse_block(lines, index + 1, indent + 2)
        else:
            value = _parse_scalar(raw)
            index += 1
        obj[key] = value
    return obj, index


def _parse_list(lines: list[str], index: int, indent: int) -> tuple[list[Any], int]:
    items: list[Any] = []
    while index < len(lines):
        if not _content(lines[index]):
            index += 1
            continue
        line_indent = _indent(lines[index])
        if line_indent < indent:
            break
        if line_indent != indent:
            raise ValueError(f"unexpected list indentation at line {index + 1}: {lines[index]}")
        stripped = _stripped(lines[index])
        if not stripped.startswith("- "):
            break
        item_text = stripped[2:].strip()
        if item_text == "":
            value, index = _parse_block(lines, index + 1, indent + 2)
            items.append(value)
            continue
        if _looks_like_key_value(item_text):
            key, raw = _split_key_value(item_text, index)
            item: dict[str, Any] = {}
            if raw == "|":
                value, next_index = _parse_block_scalar(lines, index + 1, indent + 2)
            elif raw == "":
                value, next_index = _parse_block(lines, index + 1, indent + 2)
            else:
                value = _parse_scalar(raw)
                next_index = index + 1
            item[key] = value
            if next_index < len(lines) and _indent(lines[next_index]) == indent + 2:
                extra, next_index = _parse_dict(lines, next_index, indent + 2)
                item.update(extra)
            items.append(item)
            index = next_index
        else:
            items.append(_parse_scalar(item_text))
            index += 1
    return items, index


def _parse_block_scalar(lines: list[str], index: int, indent: int) -> tuple[str, int]:
    block: list[str] = []
    while index < len(lines):
        if not lines[index].strip():
            block.append("")
            index += 1
            continue
        line_indent = _indent(lines[index])
        if line_indent < indent:
            break
        block.append(lines[index][indent:])
        index += 1
    return "\n".join(block).rstrip() + "\n", index


def _parse_scalar(raw: str) -> Any:
    if raw in {"null", "Null", "NULL", "~"}:
        return None
    if raw in {"true", "True", "TRUE"}:
        return True
    if raw in {"false", "False", "FALSE"}:
        return False
    try:
        return int(raw)
    except ValueError:
        pass
    if raw.startswith("[") and raw.endswith("]"):
        try:
            return ast.literal_eval(raw)
        except (SyntaxError, ValueError):
            return raw
    if raw == "{}":
        return {}
    if (raw.startswith('"') and raw.endswith('"')) or (raw.startswith("'") and raw.endswith("'")):
        try:
            return ast.literal_eval(raw)
        except (SyntaxError, ValueError):
            return raw[1:-1]
    return raw


def _dump(value: Any, indent: int) -> list[str]:
    pad = " " * indent
    if isinstance(value, dict):
        lines: list[str] = []
        for key, item in value.items():
            if item == []:
                lines.append(f"{pad}{key}: []")
            elif item == {}:
                lines.append(f"{pad}{key}: {{}}")
            elif isinstance(item, (dict, list)):
                lines.append(f"{pad}{key}:")
                lines.extend(_dump(item, indent + 2))
            elif isinstance(item, str) and "\n" in item:
                lines.append(f"{pad}{key}: |")
                for block_line in item.rstrip("\n").splitlines():
                    lines.append(f"{pad}  {block_line}")
            else:
                lines.append(f"{pad}{key}: {_format_scalar(item)}")
        return lines
    if isinstance(value, list):
        lines = []
        for item in value:
            if isinstance(item, dict):
                if not item:
                    lines.append(f"{pad}- {{}}")
                    continue
                first = True
                for key, child in item.items():
                    prefix = "- " if first else "  "
                    if child == []:
                        lines.append(f"{pad}{prefix}{key}: []")
                    elif child == {}:
                        lines.append(f"{pad}{prefix}{key}: {{}}")
                    elif isinstance(child, (dict, list)):
                        lines.append(f"{pad}{prefix}{key}:")
                        lines.extend(_dump(child, indent + 4))
                    else:
                        lines.append(f"{pad}{prefix}{key}: {_format_scalar(child)}")
                    first = False
            elif isinstance(item, list):
                lines.append(f"{pad}-")
                lines.extend(_dump(item, indent + 2))
            else:
                lines.append(f"{pad}- {_format_scalar(item)}")
        return lines
    return [f"{pad}{_format_scalar(value)}"]


def _format_scalar(value: Any) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        return str(value)
    return json.dumps(str(value), ensure_ascii=False)


def _split_key_value(stripped: str, index: int) -> tuple[str, str]:
    if ":" not in stripped:
        raise ValueError(f"expected key: value at line {index + 1}")
    key, raw = stripped.split(":", 1)
    return key.strip(), raw.strip()


def _looks_like_key_value(text: str) -> bool:
    if ":" not in text:
        return False
    key = text.split(":", 1)[0]
    return bool(key) and all(ch.isalnum() or ch in "_-" for ch in key)


def _content(line: str) -> bool:
    stripped = line.strip()
    return bool(stripped) and not stripped.startswith("#")


def _skip_blank(lines: list[str], index: int) -> int:
    while index < len(lines) and not _content(lines[index]):
        index += 1
    return index


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _stripped(line: str) -> str:
    return line.strip()
