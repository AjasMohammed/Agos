"""
Tests for the @tool decorator.

Covers: manifest generation, JSON Schema inference, registration.
"""
from __future__ import annotations

import pytest

import agentos
from agentos import MockKernel
from agentos.exceptions import ToolError
from agentos.tool import _annotation_to_schema, _sig_to_json_schema


class TestToolDecorator:
    def test_decorator_attaches_manifest(self):
        @agentos.tool(
            name="word-count",
            description="Count words in text",
            permissions=["memory.read:r"],
        )
        async def word_count(text: str) -> dict:
            return {"count": len(text.split())}

        assert hasattr(word_count, "_agentos_tool")
        assert word_count._agentos_tool is True
        assert hasattr(word_count, "_agentos_manifest")

    def test_manifest_fields(self):
        @agentos.tool(
            name="my-tool",
            description="A test tool",
            permissions=["fs.user_data:r"],
            trust_tier="community",
        )
        async def my_tool(x: int, y: str = "default") -> str:
            return f"{x} {y}"

        m = my_tool._agentos_manifest
        assert m["name"] == "my-tool"
        assert m["description"] == "A test tool"
        assert m["trust_tier"] == "community"
        assert "fs.user_data:r" in m["permissions"]
        assert "input_schema" in m

    def test_manifest_input_schema_types(self):
        @agentos.tool(name="typed", description="typed tool")
        async def typed_tool(name: str, count: int, ratio: float, active: bool) -> str:
            return ""

        schema = typed_tool._agentos_manifest["input_schema"]
        props = schema["properties"]
        assert props["name"]["type"] == "string"
        assert props["count"]["type"] == "integer"
        assert props["ratio"]["type"] == "number"
        assert props["active"]["type"] == "boolean"

    def test_manifest_required_vs_optional(self):
        @agentos.tool(name="defaults", description="optional params")
        async def with_defaults(required_arg: str, optional_arg: str = "x") -> str:
            return ""

        schema = with_defaults._agentos_manifest["input_schema"]
        assert "required_arg" in schema["required"]
        assert "optional_arg" not in schema["required"]

    async def test_decorator_preserves_function_behavior(self):
        """Decorated function must still be callable and awaitable."""
        @agentos.tool(name="noop", description="noop")
        async def noop(x: int) -> int:
            return x * 2

        result = await noop(21)
        assert result == 42

    def test_decorator_without_permissions(self):
        @agentos.tool(name="open-tool", description="no perms")
        async def open_tool(text: str) -> str:
            return text

        assert open_tool._agentos_manifest["permissions"] == []

    def test_sync_function_raises_type_error(self):
        """@tool must be applied to an async function."""
        with pytest.raises(TypeError, match="async function"):
            @agentos.tool(name="sync-tool", description="sync")
            def sync_tool(x: str) -> str:  # not async
                return x


class TestAnnotationToSchema:
    def test_str(self):
        assert _annotation_to_schema(str) == {"type": "string"}

    def test_int(self):
        assert _annotation_to_schema(int) == {"type": "integer"}

    def test_float(self):
        assert _annotation_to_schema(float) == {"type": "number"}

    def test_bool(self):
        assert _annotation_to_schema(bool) == {"type": "boolean"}

    def test_dict(self):
        assert _annotation_to_schema(dict) == {"type": "object"}

    def test_list(self):
        assert _annotation_to_schema(list) == {"type": "array"}

    def test_list_of_str(self):
        from typing import List
        schema = _annotation_to_schema(List[str])
        assert schema == {"type": "array", "items": {"type": "string"}}

    def test_optional_str(self):
        from typing import Optional
        schema = _annotation_to_schema(Optional[str])
        assert schema == {"oneOf": [{"type": "string"}, {"type": "null"}]}


class TestManifestToToml:
    """Verify _manifest_to_toml produces ToolManifest-compatible TOML.

    Uses structural string checks (Python 3.10 doesn't have tomllib in stdlib).
    The critical properties are section names and executor type, since those
    are what the Rust TOML deserializer validates.
    """

    def _parse(self, toml_str: str) -> dict:
        """Parse TOML using tomllib (3.11+) or tomli backport if available."""
        try:
            import tomllib  # Python 3.11+
            return tomllib.loads(toml_str)
        except ImportError:
            pass
        try:
            import tomli  # backport for Python < 3.11
            return tomli.loads(toml_str)
        except ImportError:
            pass
        # Neither available — fall back to structural string assertions
        return {}

    def test_toml_has_required_sections(self):
        """_manifest_to_toml must produce all sections ToolManifest expects."""
        from agentos.agent import _manifest_to_toml

        manifest = {
            "name": "word-count",
            "description": "Count words",
            "version": "1.0.0",
            "trust_tier": "community",
            "permissions": ["memory.read:r"],
        }
        toml_str = _manifest_to_toml(manifest)

        # All required ToolManifest sections must appear as TOML headers
        for section in ["[manifest]", "[capabilities_required]",
                        "[capabilities_provided]", "[intent_schema]",
                        "[sandbox]", "[executor]"]:
            assert section in toml_str, f"Missing section {section}"

        # ToolInfo required fields under [manifest]
        assert 'name = "word-count"' in toml_str
        assert 'description = "Count words"' in toml_str
        assert 'version = "1.0.0"' in toml_str
        # TrustTier is rename_all="lowercase" → must be lowercase
        assert 'trust_tier = "community"' in toml_str
        # author is required by ToolInfo
        assert "author" in toml_str

        # capabilities_required permissions
        assert '"memory.read:r"' in toml_str

        # executor type must be a valid ExecutorType (rename_all="lowercase")
        # "python" is NOT a valid ExecutorType — only "inline" and "wasm"
        assert 'type = "inline"' in toml_str
        assert 'type = "python"' not in toml_str

        # sandbox must have all required bool/int fields
        assert "network = false" in toml_str
        assert "fs_write = false" in toml_str
        assert "max_memory_mb" in toml_str
        assert "max_cpu_ms" in toml_str

    def test_toml_empty_permissions(self):
        """Empty permissions list produces valid TOML array."""
        from agentos.agent import _manifest_to_toml

        toml_str = _manifest_to_toml({"name": "t", "description": "d", "permissions": []})
        assert "permissions = []" in toml_str

    def test_toml_special_chars_in_description(self):
        """Descriptions with quotes and backslashes produce syntactically valid output."""
        from agentos.agent import _manifest_to_toml

        desc = 'A tool with "quotes" and a backslash\\ inside'
        toml_str = _manifest_to_toml({"name": "t", "description": desc, "permissions": []})
        # The description must appear JSON-escaped inside the TOML string
        import json
        assert f"description = {json.dumps(desc)}" in toml_str

    def test_trust_tier_is_lowercase(self):
        """trust_tier value must be lowercase to match TrustTier serde rename_all."""
        from agentos.agent import _manifest_to_toml

        # Even if passed with different capitalisation, output must be lowercase
        toml_str = _manifest_to_toml({
            "name": "t", "description": "d",
            "trust_tier": "Community",  # capitalised input
            "permissions": [],
        })
        assert 'trust_tier = "community"' in toml_str


class TestToolRegistration:
    async def test_register_tool_succeeds(self):
        @agentos.tool(name="register-me", description="tool for registration test")
        async def register_me(text: str) -> str:
            return text.upper()

        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("tool-agent")
            # Should not raise
            await agent.register_tool(register_me)

    async def test_register_undecorated_raises(self):
        async def plain_fn(x: str) -> str:
            return x

        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("tool-agent")
            with pytest.raises(ToolError, match="@agentos.tool"):
                await agent.register_tool(plain_fn)
