"""
@tool decorator — register Python functions as AgentOS tools.

The decorator inspects the function's type annotations and generates a
JSON Schema for the input, which can be submitted to the kernel via
the Agent.register_tool() method.

Example:
    @agentos.tool(
        name="word-count",
        description="Count the number of words in a text string",
        permissions=[],
    )
    async def word_count(text: str) -> dict:
        words = text.split()
        return {"count": len(words), "text_preview": text[:50]}
"""
from __future__ import annotations

import inspect
import types
from functools import wraps
from typing import Any, Callable, get_args, get_origin


def tool(
    name: str,
    description: str,
    *,
    permissions: list[str] | None = None,
    trust_tier: str = "core",
    version: str = "1.0.0",
) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
    """
    Decorator that marks a Python async function as an AgentOS tool.

    The decorated function gains:
      fn._agentos_tool       — True marker
      fn._agentos_manifest   — dict with name, description, input_schema, etc.

    The manifest can be submitted to the kernel via Agent.register_tool(fn).
    """
    if permissions is None:
        permissions = []

    def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
        if not inspect.iscoroutinefunction(fn):
            raise TypeError(
                f"@tool requires an async function, got {fn.__name__!r}. "
                "Define your tool as: async def my_tool(...): ..."
            )
        sig = inspect.signature(fn)
        input_schema = _sig_to_json_schema(sig, fn)

        manifest: dict[str, Any] = {
            "name": name,
            "description": description,
            "version": version,
            "trust_tier": trust_tier,
            "permissions": list(permissions),
            "input_schema": input_schema,
        }

        @wraps(fn)
        async def wrapper(*args: Any, **kwargs: Any) -> Any:
            return await fn(*args, **kwargs)

        wrapper._agentos_tool = True  # type: ignore[attr-defined]
        wrapper._agentos_manifest = manifest  # type: ignore[attr-defined]
        return wrapper

    return decorator


# ---------------------------------------------------------------------------
# JSON Schema generation from Python type annotations
# ---------------------------------------------------------------------------

def _sig_to_json_schema(sig: inspect.Signature, fn: Any = None) -> dict[str, Any]:
    """
    Convert a Python function signature to a JSON Schema object.

    Uses `typing.get_type_hints()` to resolve annotations that may be
    strings due to `from __future__ import annotations` in the caller's module.
    """
    import typing

    # Resolve annotations to actual types (handles PEP 563 string annotations)
    resolved: dict[str, Any] = {}
    if fn is not None:
        try:
            resolved = typing.get_type_hints(fn)
        except Exception:  # noqa: BLE001
            pass

    properties: dict[str, Any] = {}
    required: list[str] = []

    for param_name, param in sig.parameters.items():
        if param_name in ("self", "cls"):
            continue
        # Prefer resolved type hint over raw annotation (handles string annotations)
        annotation = resolved.get(param_name, param.annotation)
        prop = _annotation_to_schema(annotation)
        properties[param_name] = prop
        if param.default is inspect.Parameter.empty:
            required.append(param_name)

    schema: dict[str, Any] = {"type": "object", "properties": properties}
    if required:
        schema["required"] = required
    return schema


def _annotation_to_schema(annotation: Any) -> dict[str, Any]:
    """Map a Python type annotation to a JSON Schema dict."""
    if annotation is inspect.Parameter.empty or annotation is None:
        return {}

    # Handle None type
    if annotation is type(None):
        return {"type": "null"}

    # Handle basic types
    _PRIMITIVES: dict[Any, str] = {
        str: "string",
        int: "integer",
        float: "number",
        bool: "boolean",
        bytes: "string",
    }
    if annotation in _PRIMITIVES:
        return {"type": _PRIMITIVES[annotation]}

    if annotation is dict or annotation is Any:
        return {"type": "object"}

    if annotation is list:
        return {"type": "array"}

    origin = get_origin(annotation)
    args = get_args(annotation)

    # Optional[X] → Union[X, None]
    if origin is types.UnionType or (
        hasattr(types, "UnionType") and isinstance(annotation, types.UnionType)
    ):
        non_none = [a for a in args if a is not type(None)]
        if len(non_none) == 1:
            base = _annotation_to_schema(non_none[0])
            # JSON Schema draft-07: mark as nullable via oneOf
            return {"oneOf": [base, {"type": "null"}]}
        return {}

    # typing.Union
    try:
        import typing
        if origin is typing.Union:
            non_none = [a for a in args if a is not type(None)]
            if len(non_none) == 1:
                base = _annotation_to_schema(non_none[0])
                return {"oneOf": [base, {"type": "null"}]}
            return {}
    except Exception:  # noqa: BLE001
        pass

    # list[X]
    if origin is list:
        item_schema = _annotation_to_schema(args[0]) if args else {}
        return {"type": "array", "items": item_schema}

    # dict[K, V]
    if origin is dict:
        return {"type": "object"}

    # Fallback
    return {}
