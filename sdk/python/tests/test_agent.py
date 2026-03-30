"""
Tests for Agent class using MockKernel.

Covers: connect, run, list_tasks, grant_permission, disconnect.
"""
from __future__ import annotations

import pytest

from agentos import Agent, MockKernel
from agentos.types import TaskState
from agentos.exceptions import KernelCommandError, TaskError


class TestAgentConnect:
    async def test_connect_returns_agent(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("test-agent")
            assert agent.name == "test-agent"
            assert len(agent.agent_id) > 0

    async def test_connect_reconnect_same_id(self):
        async with MockKernel() as kernel:
            a1 = await kernel.connect_agent("alpha")
            a2 = await kernel.connect_agent("alpha")
            assert a1.agent_id == a2.agent_id

    async def test_connect_different_agents_different_ids(self):
        async with MockKernel() as kernel:
            a1 = await kernel.connect_agent("alpha")
            a2 = await kernel.connect_agent("beta")
            assert a1.agent_id != a2.agent_id


class TestAgentRun:
    async def test_run_returns_task_result(self):
        async with MockKernel() as kernel:
            kernel.set_llm_response("The answer is 42.")
            agent = await kernel.connect_agent("calc")
            result = await agent.run("What is 6 * 7?")
            assert result.success
            assert result.output == "The answer is 42."
            assert len(result.task_id) > 0

    async def test_run_records_task(self):
        async with MockKernel() as kernel:
            kernel.set_llm_response("done")
            agent = await kernel.connect_agent("worker")
            await agent.run("do something")
            assert len(kernel._tasks) == 1
            assert kernel._tasks[0]["prompt"] == "do something"

    async def test_run_multiple_tasks(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("multi")
            for i in range(3):
                kernel.set_llm_response(f"result {i}")
                result = await agent.run(f"task {i}")
                assert result.success
            assert len(kernel._tasks) == 3

    async def test_run_error_raises_task_error(self):
        async with MockKernel() as kernel:
            async def fail_handler(**_):
                raise KernelCommandError("task exploded")

            kernel.add_command_handler("RunTask", fail_handler)
            agent = await kernel.connect_agent("breaker")
            with pytest.raises(TaskError, match="task exploded"):
                await agent.run("break it")


class TestAgentPermissions:
    async def test_grant_permission_succeeds(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("agent")
            # Should not raise
            await agent.grant_permission("fs.user_data:rw")

    async def test_show_permissions_returns_dict(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("agent")
            perms = await agent.show_permissions()
            assert isinstance(perms, dict)


class TestAgentListTasks:
    async def test_list_tasks_empty_initially(self):
        async with MockKernel() as kernel:
            agent = await kernel.connect_agent("agent")
            tasks = await agent.list_tasks()
            assert tasks == []

    async def test_list_tasks_after_run(self):
        async with MockKernel() as kernel:
            kernel.set_llm_response("ok")
            agent = await kernel.connect_agent("agent")
            await agent.run("do it")
            tasks = await agent.list_tasks()
            assert len(tasks) == 1
            assert tasks[0].state == TaskState.Complete


class TestAgentContextManager:
    async def test_context_manager_closes_cleanly(self):
        async with MockKernel() as kernel:
            async with await kernel.connect_agent("cm-agent") as agent:
                result = await agent.run("hello")
                assert result.success
