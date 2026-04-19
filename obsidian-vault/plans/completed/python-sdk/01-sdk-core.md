---
title: Python SDK Core — @tool decorator, Param, run()
tags:
  - sdk
  - python
  - phase-1
date: 2026-04-14
status: complete
effort: 1d
priority: high
---

# Phase 1 — Python SDK Core

> Extend `sdk/python/agentos/tool.py` with `Param()` descriptors, sync function
> support, and a `run()` entry point that handles the standalone script lifecycle.

---

## Why this phase

The existing `@tool` decorator is async-only and has no standalone execution
path.  Users who want to write file-based tools (installed via
`agentos tool install file.py`) need:

1. **`Param()`** — rich per-parameter metadata (description, default, enum,
   min, max) that feeds directly into the generated JSON Schema, so the LLM
   receives accurate call signatures.
2. **Sync function support** — scripts run as subprocesses; `asyncio.run()` can
   be added by the SDK when needed, but force-async is a friction for simple tools.
3. **`run()` method** — single entry point on the wrapper that handles:
   - `--agentos-manifest` → print JSON manifest and exit (used by the CLI to
     extract metadata without in-process import)
   - Otherwise → read `AGENTOS_INPUT` env var, parse JSON, call the function,
     print JSON result to stdout

---

## Current → Target state

| Before | After |
|--------|-------|
| `@tool` requires `async def` | `@tool` supports both sync and async |
| No `Param()` descriptor | `Param(description=, default=, enum=, minimum=, maximum=)` |
| No standalone runner | `fn.run()` handles I/O boilerplate |
| No `fn_name` in manifest | `manifest["fn_name"]` = original Python function name |
| No `--agentos-manifest` support | `fn.run()` prints manifest JSON and exits |

---

## Files changed

| File | Change |
|------|--------|
| `sdk/python/agentos/tool.py` | Add `Param` class, sync support, `run()`, `fn_name` |
| `sdk/python/agentos/__init__.py` | Export `Param` |
| `sdk/python/tests/test_tool.py` | Update sync test, add `TestParam` class |

---

## Detailed subtasks

### 1. Add `Param` class

```python
_MISSING = object()  # sentinel

class Param:
    def __init__(self, *, description="", default=_MISSING,
                 enum=None, minimum=None, maximum=None): ...

    @property
    def has_default(self) -> bool: ...

    @property
    def default(self) -> Any: ...
```

Place above the `tool()` function in `tool.py`.

### 2. Support sync functions

Remove the `if not inspect.iscoroutinefunction(fn): raise TypeError(...)` check.
Instead, set `is_async = inspect.iscoroutinefunction(fn)` and branch:

```python
if is_async:
    @wraps(fn)
    async def wrapper(*args, **kwargs): return await fn(*args, **kwargs)
else:
    @wraps(fn)
    def wrapper(*args, **kwargs): return fn(*args, **kwargs)
```

### 3. Add `fn_name` to manifest

```python
manifest = {
    "name": name,
    "fn_name": fn.__name__,   # ← new
    ...
}
```

### 4. Add `run()` method

```python
def run() -> None:
    if "--agentos-manifest" in sys.argv:
        print(json.dumps(wrapper._agentos_manifest))
        sys.exit(0)
    raw = os.environ.get("AGENTOS_INPUT", "{}")
    args_dict = json.loads(raw)
    result = asyncio.run(fn(**args_dict)) if is_async else fn(**args_dict)
    if not isinstance(result, (dict, list)):
        result = {"result": result}
    print(json.dumps(result))

wrapper.run = run
```

### 5. Update `_sig_to_json_schema` for `Param`

When `param.default` is a `Param` instance:
- Copy `description`, `enum`, `minimum`, `maximum` into the property schema
- If `Param.has_default` is `False` → add to `required`
- If `Param.has_default` is `True` → add `prop["default"] = param.default.default`

### 6. Export `Param` from `__init__.py`

```python
from .tool import Param, tool
# add "Param" to __all__
```

### 7. Update tests

- Replace `test_sync_function_raises_type_error` with `test_sync_function_works`
- Add `TestParam` class with tests for:
  - `Param` without default → required
  - `Param` with default → optional, default in schema
  - `Param` with enum/min/max → present in schema
  - `run()` with `--agentos-manifest` → prints manifest JSON, exits 0

---

## Verification

```bash
cd sdk/python
pip install -e .
python -m pytest tests/test_tool.py -v
```

All tests must pass. Specifically:
- `TestParam::test_param_required_when_no_default` — city in required[]
- `TestParam::test_param_optional_when_default_given` — units NOT in required
- `TestToolDecorator::test_sync_function_works` — no TypeError
- `TestToolDecorator::test_run_method_manifest_flag` — manifest JSON printed

---

## Dependencies

None — this is Phase 1.

## Blocks

[[02-cli-integration]], [[03-schema-surfacing]]
